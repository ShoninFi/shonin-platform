//! The reference indexer.
//!
//! It follows every vault chain and turns each vault's value per share into a
//! record, holding one snapshot that carries everything a page renders, so
//! every figure on the screen comes from the same moment. It needs no enclave and
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
use chain::Served;
use shared::Chain;
use shared::model::health::Health;
use shared::model::registry::{Author, FeedEvent, Job};
use shared::{ChainInfo, Snapshot, Totals};
use tokio::sync::RwLock;

use crate::read::History;
use crate::store::Store;

/// One vault chain, as the indexer reads it.
pub struct Reader {
    pub served: Served,
    history: RwLock<History>,
}

/// What one chain read came to.
struct Read {
    info: ChainInfo,
    now: u64,
    strategies: Vec<shared::Strategy>,
    authors: Vec<Author>,
    feed: Vec<FeedEvent>,
}

/// The record, as read from every vault chain, and what it needs to keep
/// reading.
pub struct Indexer {
    pub readers: Vec<Reader>,
    pub store: Store,
    /// Where the build service answers, for the queue. Without one the record
    /// is the chains alone and the queue reads as empty.
    pub builder: Option<String>,
    pub snapshot: RwLock<Snapshot>,
    /// Health reports, as executors post them, by vault chain and vault: the
    /// same address can be a different vault on another chain.
    pub health: RwLock<HashMap<(Chain, Address), Health>>,
}

impl Indexer {
    pub fn new(served: Vec<Served>, store: PathBuf, builder: Option<String>) -> Self {
        Self {
            snapshot: RwLock::new(empty_snapshot(&served)),
            readers: served
                .into_iter()
                .map(|served| Reader {
                    served,
                    history: RwLock::new(History::default()),
                })
                .collect(),
            health: RwLock::new(HashMap::new()),
            store: Store::new(store),
            builder,
        }
    }

    /// The chain a vault chain is read from.
    pub fn on(&self, chain: Chain) -> Option<&Served> {
        self.readers
            .iter()
            .map(|reader| &reader.served)
            .find(|served| served.vault_chain == chain)
    }

    /// Keep reading, every couple of seconds, for as long as the process runs.
    pub async fn run(&self) {
        loop {
            if let Err(error) = self.refresh().await {
                tracing::warn!(%error, "could not read the chains");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Read everything again, from every chain. It is cheap enough to do
    /// often and simple enough to be obviously correct, which matters more
    /// here than being clever.
    pub async fn refresh(&self) -> Result<()> {
        let health = self.health.read().await.clone();
        let reads = futures::future::join_all(
            self.readers
                .iter()
                .map(|reader| self.read_one(reader, &health)),
        )
        .await;

        let reads: Vec<Read> = self
            .readers
            .iter()
            .zip(reads)
            .filter_map(|(reader, read)| match read {
                Ok(read) => Some(read),
                Err(error) => {
                    tracing::warn!(chain = %reader.served.key, %error, "could not read this chain");
                    None
                }
            })
            .collect();
        let jobs = self.jobs().await.unwrap_or_default();

        let mut infos = Vec::new();
        let mut latest = 0;
        let mut strategies = Vec::new();
        let mut authors: Vec<Author> = Vec::new();
        let mut feed = Vec::new();
        for read in reads {
            infos.push(read.info);
            latest = latest.max(read.now);
            strategies.extend(read.strategies);
            feed.extend(read.feed);
            // An author is one handle, whichever chains they publish on.
            for author in read.authors {
                if !authors.iter().any(|known| known.handle == author.handle) {
                    authors.push(author);
                }
            }
        }
        feed.sort_by_key(|event| std::cmp::Reverse(event.at));

        let totals = Totals {
            // Every vault still holding depositors' money.
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
        snapshot.as_of = read::moment(latest);
        snapshot.chains = infos;
        snapshot.strategies = strategies;
        snapshot.authors = authors;
        snapshot.jobs = jobs;
        snapshot.feed = feed;
        snapshot.totals = totals;
        Ok(())
    }

    async fn read_one(
        &self,
        reader: &Reader,
        health: &HashMap<(Chain, Address), Health>,
    ) -> Result<Read> {
        let served = &reader.served;
        let chain = &served.client;
        let head = chain.block_number().await?;
        let now = chain.block_timestamp().await?;
        let health: HashMap<Address, Health> = health
            .iter()
            .filter(|((on, _), _)| *on == served.vault_chain)
            .map(|((_, vault), report)| (*vault, report.clone()))
            .collect();

        let strategies = {
            let mut history = reader.history.write().await;
            read::strategies(
                chain,
                served.vault_chain,
                &served.deployments,
                &self.store,
                &mut history,
                &health,
                head,
                now,
            )
            .await?
        };
        let authors = read::authors(chain, &served.deployments).await?;
        let feed = read::feed(chain, served.vault_chain, &served.deployments, &strategies).await?;
        Ok(Read {
            info: ChainInfo {
                chain: served.vault_chain,
                chain_id: chain.chain_id,
                block: head,
            },
            now,
            strategies,
            authors,
            feed,
        })
    }

    /// The builder's queue, with what the chain adds to each job.
    async fn jobs(&self) -> Result<Vec<Job>> {
        let Some(builder) = &self.builder else {
            return Ok(Vec::new());
        };
        let jobs: Vec<Job> = reqwest::Client::new()
            .get(format!("{builder}/jobs"))
            .send()
            .await?
            .json()
            .await?;
        let mut out = Vec::with_capacity(jobs.len());
        for job in jobs {
            out.push(match self.on(job.chain) {
                Some(served) => {
                    read::job(&served.client, &served.deployments, &self.store, job).await
                }
                None => job,
            });
        }
        Ok(out)
    }
}

fn empty_snapshot(served: &[Served]) -> Snapshot {
    Snapshot {
        version: 0,
        as_of: read::moment(0),
        chains: served
            .iter()
            .map(|served| ChainInfo {
                chain: served.vault_chain,
                chain_id: served.client.chain_id,
                block: 0,
            })
            .collect(),
        strategies: Vec::new(),
        authors: Vec::new(),
        jobs: Vec::new(),
        feed: Vec::new(),
        totals: Totals {
            vault_capital: 0.0,
            trades: 0,
        },
    }
}
