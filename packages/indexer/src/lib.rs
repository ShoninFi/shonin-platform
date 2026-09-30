//! The reference indexer.
//!
//! It follows the chain and turns each vault's value per share into a record,
//! holding one snapshot that carries everything a page renders, so every
//! figure on the screen comes from the same moment. It needs no enclave and
//! holds no key: anyone can run their own and compare.
//!
//! The binary in this crate only reads. The web app's API runs the same
//! indexer in-process and serves what it holds.

pub mod read;
pub mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use alloy::primitives::Address;
use anyhow::Result;
use chain::{Client, Deployments};
use shared::model::health::Health;
use shared::model::registry::Job;
use shared::{ChainInfo, Snapshot, Totals};
use tokio::sync::RwLock;

use crate::read::History;
use crate::store::Store;

/// The record, as read from one chain, and what it needs to keep reading.
pub struct Indexer {
    pub chain: Client,
    pub deployments: Deployments,
    pub store: Store,
    /// Where the build service answers, for the queue. Without one the record
    /// is the chain alone and the queue reads as empty.
    pub builder: Option<String>,
    pub snapshot: RwLock<Snapshot>,
    history: RwLock<History>,
    /// Health reports, as executors post them.
    pub health: RwLock<HashMap<Address, Health>>,
}

impl Indexer {
    pub fn new(
        chain: Client,
        deployments: Deployments,
        store: PathBuf,
        builder: Option<String>,
    ) -> Self {
        Self {
            snapshot: RwLock::new(empty_snapshot(&chain)),
            history: RwLock::new(History::default()),
            health: RwLock::new(HashMap::new()),
            store: Store::new(store),
            builder,
            chain,
            deployments,
        }
    }

    /// Keep reading, every couple of seconds, for as long as the process runs.
    pub async fn run(&self) {
        loop {
            if let Err(error) = self.refresh().await {
                tracing::warn!(%error, "could not read the chain");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Read everything again. It is cheap enough to do often and simple
    /// enough to be obviously correct, which matters more here than being
    /// clever.
    pub async fn refresh(&self) -> Result<()> {
        let head = self.chain.block_number().await?;
        let now = self.chain.block_timestamp().await?;
        let health = self.health.read().await.clone();

        let strategies = {
            let mut history = self.history.write().await;
            read::strategies(
                &self.chain,
                &self.deployments,
                &self.store,
                &mut history,
                &health,
                head,
                now,
            )
            .await?
        };
        let authors = read::authors(&self.chain, &self.deployments).await?;
        let feed = read::feed(&self.chain, &self.deployments, &strategies).await?;
        let benchmarks = {
            let mut history = self.history.write().await;
            read::benchmark(&self.chain, &self.deployments, &mut history, head).await?
        };
        let jobs = self.fetch_jobs().await.unwrap_or_default();

        let totals = Totals {
            // Every vault still holding depositors' money, which includes one
            // whose author has published a successor: it carries on as it was.
            vault_capital: strategies
                .iter()
                .filter(|strategy| {
                    matches!(
                        strategy.vault.state,
                        shared::model::vault::VaultState::Open
                            | shared::model::vault::VaultState::Frozen
                    )
                })
                .map(|strategy| strategy.vault.value)
                .sum(),
            trades: strategies
                .iter()
                .map(|strategy| strategy.trades.len() as u32)
                .sum(),
        };

        let mut snapshot = self.snapshot.write().await;
        snapshot.version += 1;
        snapshot.as_of = read::moment(now);
        snapshot.chain.block = head;
        snapshot.strategies = strategies;
        snapshot.authors = authors;
        snapshot.jobs = jobs;
        snapshot.feed = feed;
        snapshot.benchmarks = benchmarks;
        snapshot.totals = totals;
        Ok(())
    }

    async fn fetch_jobs(&self) -> Result<Vec<Job>> {
        let Some(builder) = &self.builder else {
            return Ok(Vec::new());
        };
        let jobs = reqwest::Client::new()
            .get(format!("{builder}/jobs"))
            .send()
            .await?
            .json()
            .await?;
        Ok(jobs)
    }
}

fn empty_snapshot(chain: &Client) -> Snapshot {
    Snapshot {
        version: 0,
        as_of: read::moment(0),
        chain: ChainInfo {
            chain: shared::model::common::Chain::Local,
            chain_id: chain.chain_id,
            block: 0,
        },
        strategies: Vec::new(),
        authors: Vec::new(),
        jobs: Vec::new(),
        feed: Vec::new(),
        benchmarks: Vec::new(),
        totals: Totals {
            vault_capital: 0.0,
            trades: 0,
        },
    }
}
