//! One EVM chain's record, read through the shared chain follower.
//!
//! Each refresh:
//!
//! 1. reads the head's header; a head that fails to read, or one below the
//!    head read before (an endpoint behind), skips the refresh: nothing is
//!    published and the record's time is not renewed;
//! 2. observes, pinned to that header, every view the record read last time
//!    (learned from the code that reads them: see [`crate::views`]), in as
//!    few batched calls as the endpoint allows, and checks the observation
//!    against the anchors the follower holds: a replaced branch restores the
//!    newest checkpoint at or below the fork, and one replaced deeper than
//!    the checkpoints reach reads everything again from the deployment;
//! 3. reads the logs from where the last refresh stopped up to the head, in
//!    pages over every watched address at once, each page checked against
//!    the followed branch, and folds them into what is known
//!    ([`History::fold`]); a vault the factory lists that the logs were not
//!    yet read for is read from the block it was created in until it has
//!    caught up, then watched with the rest;
//! 4. takes the readings due at past blocks (each vault's hourly value, the
//!    supply a paid claim was paid from), each in one observation pinned to
//!    that block's header;
//! 5. builds the record from the observation and what the logs come to; a
//!    view the observation did not carry is observed at the same block, and
//!    the record built again;
//! 6. keeps what it all came to as a checkpoint, in a ring.
//!
//! While the logs have not been read up to the head (after a start, or after
//! a reorganisation), the record is published as catching up, dated by the
//! last block read whole, and the refreshes run back to back.

use std::collections::{BTreeSet, HashMap, VecDeque};

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use anyhow::{Result, anyhow, bail};
use chain::Deployments;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::IVault;
use chain::follow::{
    At, Continuity, Follower, Header, Item, Plan, RangeRead, RangeReader, Read as PlanRead, Watch,
};
use chain::rpc_policy::RpcPolicy;
use shared::Chain;
use shared::model::health::Health;
use shared::model::registry::Author;
use shared::model::strategy::Strategy;

use crate::read::{self, History, PastReads};
use crate::store::Store;
use crate::views::{self, Facts, Key, Views};

/// The most times one refresh observes again at the same block for views
/// the observation did not carry: each round reads what the last found
/// (the vaults, then each vault's views, then what those name).
const ROUNDS: usize = 6;

/// The most paid claims whose supply before payment is read in one refresh.
const SUPPLIES_PER_REFRESH: usize = 32;

/// What one refresh came to.
pub enum Refresh {
    /// The record as of `head`.
    Read(Box<Refreshed>),
    /// Nothing was published: why.
    Skipped(String),
}

/// One chain's record as of one block.
pub struct Refreshed {
    /// The block the views were read at.
    pub head: Header,
    /// Every watched log has been read and folded through this block.
    pub through: u64,
    /// That block's time, when it is known.
    pub through_time: Option<u64>,
    /// Whether the logs are still being read up to the head.
    pub catching_up: bool,
    pub strategies: Vec<Strategy>,
    /// Everyone who has published; `None` when they could not be read.
    pub authors: Option<Vec<(Address, Author)>>,
    pub feed: Vec<shared::model::registry::FeedEvent>,
    pub trades: u32,
    /// The platform's terms on this chain; `None` when they could not be
    /// read.
    pub terms: Option<shared::Terms>,
    /// How many payments the publishing contract has taken.
    pub payments: Option<u64>,
    /// What each holder paid for what they hold, by vault, for every vault
    /// whose logs have been read up to the head: none while catching up.
    pub costs: HashMap<Address, HashMap<Address, f64>>,
}

/// What the record has derived from the chain as of one block: kept whole
/// in each checkpoint.
#[derive(Clone)]
struct Derived {
    history: History,
    facts: Facts,
    facts_bytes: usize,
    readers: Readers,
}

/// The log readers: one over the platform's contracts and every vault
/// caught up with it, and one for each vault still reading its earlier
/// logs.
#[derive(Clone)]
struct Readers {
    main: RangeReader,
    /// The platform's contracts the main reader watches.
    platform: Vec<Address>,
    /// The vaults it watches.
    vaults: BTreeSet<Address>,
    /// Vaults reading their logs up to where the main reader is, each from
    /// the block it was created in.
    joining: Vec<(Address, RangeReader)>,
    first: u64,
}

impl Readers {
    fn new(deployments: &Deployments, policy: &RpcPolicy) -> Self {
        let mut platform = vec![
            deployments.factory,
            deployments.strategies,
            deployments.attestation,
        ];
        if deployments.venue != Address::ZERO {
            platform.push(deployments.venue);
        }
        platform.sort();
        platform.dedup();
        let first = deployments.deployed_block;
        Self {
            main: reader(platform.clone(), first, policy),
            platform,
            vaults: BTreeSet::new(),
            joining: Vec::new(),
            first,
        }
    }

    /// The last block the main reader has read.
    fn through(&self) -> u64 {
        self.main.next_block().saturating_sub(1)
    }

    fn watched(&self, vault: &Address) -> bool {
        self.vaults.contains(vault) || self.joining.iter().any(|(joining, _)| joining == vault)
    }

    /// Watch `vault` with the main reader from where it is now.
    fn watch(&mut self, vault: Address, policy: &RpcPolicy) {
        self.vaults.insert(vault);
        let mut addresses = self.platform.clone();
        addresses.extend(self.vaults.iter().copied());
        self.main = reader(addresses, self.main.next_block(), policy);
    }
}

fn reader(addresses: Vec<Address>, from: u64, policy: &RpcPolicy) -> RangeReader {
    RangeReader::new(
        Watch {
            addresses,
            topics: read::topics(),
        },
        from,
        policy,
    )
}

/// A checkpoint: what was derived as of `number`.
#[derive(Clone)]
struct Checkpoint {
    number: u64,
    state: Derived,
}

/// One EVM chain's record and what it needs to keep reading.
pub struct EvmRecord {
    vault_chain: Chain,
    deployments: Deployments,
    policy: RpcPolicy,
    follower: Follower,
    /// A follower of its own for reads at past blocks, so one an endpoint
    /// cannot answer (state it no longer keeps) holds nothing of the head's
    /// reads back.
    past: Follower,
    state: Derived,
    checkpoints: VecDeque<Checkpoint>,
    /// The views the record read last time: what the next observation reads.
    plan: BTreeSet<Key>,
    /// The head the last refresh read.
    head: Option<Header>,
}

impl EvmRecord {
    pub fn new(served: &chain::Served, policy: RpcPolicy) -> Self {
        Self {
            vault_chain: served.vault_chain,
            deployments: served.deployments.clone(),
            follower: Follower::new(served.client.clone(), policy.clone()),
            past: Follower::new(served.client.clone(), policy.clone()),
            state: Derived {
                history: History::default(),
                facts: Facts::new(),
                facts_bytes: 0,
                readers: Readers::new(&served.deployments, &policy),
            },
            checkpoints: VecDeque::new(),
            plan: BTreeSet::new(),
            head: None,
            policy,
        }
    }

    /// The chain's RPC settings.
    pub fn policy(&self) -> &RpcPolicy {
        &self.policy
    }

    /// Read the chain into the record once (see the module's documentation).
    pub async fn refresh(&mut self, store: &Store, health: &HashMap<Address, Health>) -> Refresh {
        match self.read(store, health).await {
            Ok(refreshed) => Refresh::Read(Box::new(refreshed)),
            Err(why) => Refresh::Skipped(format!("{why:#}")),
        }
    }

    async fn read(
        &mut self,
        store: &Store,
        health: &HashMap<Address, Health>,
    ) -> Result<Refreshed> {
        let head = self
            .follower
            .header_any(BlockNumberOrTag::Latest)
            .await
            .map_err(|error| anyhow!("the head: {error:#}"))?;
        if let Some(last) = self.head
            && head.number < last.number
        {
            bail!(
                "the endpoint's head is block {}, behind block {} read before",
                head.number,
                last.number
            );
        }
        let plan = self.plan();
        let observed = self
            .follower
            .observe(&plan, At::Pinned(head))
            .await
            .map_err(|error| anyhow!("no observation: {error:#}"))?;
        match self.follower.check(&observed).await {
            Continuity::Extends => {}
            Continuity::Replaced { fork_at, depth } => {
                tracing::warn!(chain = %self.vault_chain, fork_at, depth, "the branch the record followed was replaced; what was read of it is read again");
                self.restore(fork_at);
            }
            Continuity::Deep => {
                tracing::warn!(chain = %self.vault_chain, "the branch the record followed was replaced deeper than it can trace; everything is read again");
                self.reset();
            }
            Continuity::Lagging { number, head } => {
                bail!("the endpoint answered at block {number}, behind block {head}")
            }
            Continuity::Unknown(why) => bail!("the observation could not be checked: {why}"),
        }
        self.head = Some(head);
        let mut answers = answers_of(&plan, &observed);

        // The vaults the factory lists: watched from where the logs are, or
        // read from the block each was created in.
        let listed = Views::new(&answers, &self.state.facts)
            .read(self.deployments.factory, IVaultFactory::vaultsCall {})
            .unwrap_or_default();
        self.join(&listed);
        self.read_logs(&observed).await?;
        self.join(&listed);

        let through = self.state.readers.through();
        let read_through = through >= head.number;
        let catching_up = !read_through || !self.state.readers.joining.is_empty();

        // Readings at past blocks.
        let mut past = PastReads::default();
        self.supplies(&mut past).await;
        let mut sampler = Sampler {
            follower: &mut self.past,
        };
        for vault in &listed {
            let created_in = match self.state.history.created_in(vault) {
                Some(block) => Some(block.max(self.deployments.deployed_block)),
                None if read_through => Some(self.deployments.deployed_block),
                None => None,
            };
            read::sample(
                &mut sampler,
                *vault,
                created_in,
                &mut self.state.history,
                head,
            )
            .await;
        }

        // The record, built again at the same block for as long as it reads
        // a view the observation did not carry.
        let mut rounds = 0;
        let built = loop {
            let mut history = self.state.history.clone();
            let views = Views::new(&answers, &self.state.facts);
            let built = Built::of(
                &views,
                self.vault_chain,
                &self.deployments,
                store,
                &mut history,
                health,
                head.timestamp,
                read_through,
                &past,
            );
            let missing = views.missing();
            let asked = views.asked();
            if missing.is_empty() || rounds == ROUNDS {
                if !missing.is_empty() {
                    tracing::warn!(chain = %self.vault_chain, missing = missing.len(), "views were still missing after every round; the vaults that read them are shown as last read");
                }
                break (built, history, asked);
            }
            rounds += 1;
            let more = Plan {
                reads: missing
                    .iter()
                    .map(|(target, data)| PlanRead {
                        target: *target,
                        data: data.clone(),
                    })
                    .collect(),
                ..Plan::default()
            };
            let observed = self
                .follower
                .observe(&more, At::Pinned(head))
                .await
                .map_err(|error| anyhow!("no observation of the views missing: {error:#}"))?;
            answers.extend(answers_of(&more, &observed));
        };
        let (built, history, asked) = built;
        let built = built?;
        self.state.history = history;
        self.learn(&asked, &answers, head.number);

        let through_time = if catching_up {
            self.follower
                .header(observed.endpoint, BlockNumberOrTag::Number(through))
                .await
                .ok()
                .map(|header| header.timestamp)
        } else {
            Some(head.timestamp)
        };
        // What holders paid is known for the vaults whose logs have been read
        // up to the head; while the main reader is behind it, for none.
        let mut costs: HashMap<Address, HashMap<Address, f64>> = HashMap::new();
        if read_through {
            let all = self.state.history.costs();
            for vault in &self.state.readers.vaults {
                costs.insert(*vault, all.get(vault).cloned().unwrap_or_default());
            }
        }
        self.checkpoint(head.number);
        Ok(Refreshed {
            head,
            through: if catching_up { through } else { head.number },
            through_time,
            catching_up,
            strategies: built.strategies,
            authors: built.authors,
            feed: built.feed,
            trades: built.trades,
            terms: built.terms,
            payments: built.payments,
            costs,
        })
    }

    /// The next observation's reads: every view read last time, and the
    /// factory's list of vaults, from which every other read follows.
    fn plan(&self) -> Plan {
        let mut plan = Plan::default();
        let vaults: Key = (
            self.deployments.factory,
            IVaultFactory::vaultsCall {}.abi_encode().into(),
        );
        let mut keys = self.plan.clone();
        keys.insert(vaults);
        for (target, data) in keys {
            plan.reads.push(PlanRead { target, data });
        }
        plan
    }

    /// Learn the reads a refresh made, and keep the facts it read that no
    /// contract changes once made, as of `number`.
    fn learn(&mut self, asked: &BTreeSet<Key>, answers: &HashMap<Key, Item>, number: u64) {
        let entries = usize::try_from(self.policy.immutable_cache_entries).unwrap_or(usize::MAX);
        let bytes = usize::try_from(self.policy.immutable_cache_bytes).unwrap_or(usize::MAX);
        let mut plan = BTreeSet::new();
        for key in asked {
            if views::fixed(&key.1)
                && let Some(Item::Returned(answer)) = answers.get(key)
            {
                let size = key.1.len() + answer.len() + 20;
                if self.state.facts.len() < entries && self.state.facts_bytes + size <= bytes {
                    self.state.facts_bytes += size;
                    self.state
                        .facts
                        .insert(key.clone(), (answer.clone(), number));
                    continue;
                }
            }
            plan.insert(key.clone());
        }
        self.plan = plan;
    }

    /// Start reading the logs of every listed vault not watched yet: from
    /// where the main reader is, when nothing has been read yet or the
    /// vault's activation is still ahead of it; otherwise from the block it
    /// was created in, apart, until it has caught up.
    fn join(&mut self, listed: &[Address]) {
        let readers = &mut self.state.readers;
        let nothing_read = readers.main.next_block() == readers.first;
        let read_through = self
            .head
            .is_some_and(|head| readers.through() >= head.number);
        for vault in listed {
            if readers.watched(vault) {
                continue;
            }
            match self.state.history.created_in(vault) {
                _ if nothing_read => readers.watch(*vault, &self.policy),
                Some(created_in) => {
                    let from = created_in.max(readers.first);
                    if from >= readers.main.next_block() {
                        readers.watch(*vault, &self.policy);
                    } else {
                        readers
                            .joining
                            .push((*vault, reader(vec![*vault], from, &self.policy)));
                    }
                }
                // Not activated by this factory, as far as its log has been
                // read: once it has been read through, the vault's logs are
                // read from the deployment.
                None if read_through => {
                    readers
                        .joining
                        .push((*vault, reader(vec![*vault], readers.first, &self.policy)));
                }
                None => readers.watch(*vault, &self.policy),
            }
        }
    }

    /// Read on every reader's logs and fold them: the main reader up to the
    /// observed block, each joining vault up to where the main reader is.
    async fn read_logs(&mut self, observed: &chain::follow::Observed) -> Result<()> {
        let seen = Some((observed.endpoint, observed.number));
        let main = &mut self.state.readers.main;
        if let Some(pages) = main
            .pages(&self.follower, observed.number, seen)
            .await
            .map_err(|error| anyhow!("reading logs: {error:#}"))?
        {
            let read = main.settle(pages, &mut self.follower, observed).await;
            self.take(read)?;
        }
        let mut index = 0;
        while index < self.state.readers.joining.len() {
            let to = self.state.readers.through();
            let (vault, joining) = &mut self.state.readers.joining[index];
            let vault = *vault;
            if joining.next_block() <= to {
                let pages = joining
                    .pages(&self.follower, to, seen)
                    .await
                    .map_err(|error| anyhow!("reading the logs of vault {vault}: {error:#}"))?;
                if let Some(pages) = pages {
                    let read = joining.settle(pages, &mut self.follower, observed).await;
                    self.take(read)?;
                }
            }
            let readers = &mut self.state.readers;
            if readers
                .joining
                .get(index)
                .is_some_and(|(_, joining)| joining.next_block() > readers.through())
            {
                readers.joining.remove(index);
                readers.watch(vault, &self.policy);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    /// Fold what a reader read, or undo what a replacement it found undid.
    fn take(&mut self, read: RangeRead) -> Result<()> {
        match read {
            RangeRead::Read { logs, .. } => {
                self.state.history.fold(&self.deployments, &logs);
                Ok(())
            }
            RangeRead::Replaced { fork_at } => {
                tracing::warn!(chain = %self.vault_chain, fork_at, "logs show the branch the record followed was replaced; what was read of it is read again");
                self.restore(fork_at);
                bail!("the logs above block {fork_at} were replaced")
            }
            RangeRead::Deep => {
                self.reset();
                bail!("the logs were replaced deeper than the anchors reach")
            }
            RangeRead::Unverified(why) => bail!("the logs could not be checked: {why}"),
        }
    }

    /// Read the supply each paid claim was paid from, at the block before
    /// its payment, for a few claims a refresh.
    async fn supplies(&mut self, past: &mut PastReads) {
        for (vault, id, paid) in self
            .state
            .history
            .unmeasured_payments()
            .into_iter()
            .take(SUPPLIES_PER_REFRESH)
        {
            match supply_before(&mut self.past, vault, paid).await {
                Ok(supply) => self.state.history.measured(vault, id, supply),
                Err(error) => {
                    past.failed.insert((vault, id), format!("{error:#}"));
                }
            }
        }
    }

    /// Keep what the record has derived as of `number`, and as many before
    /// it as the ring holds.
    fn checkpoint(&mut self, number: u64) {
        self.checkpoints
            .retain(|checkpoint| checkpoint.number < number);
        self.checkpoints.push_back(Checkpoint {
            number,
            state: self.state.clone(),
        });
        let keep = usize::try_from(self.policy.record_checkpoints)
            .unwrap_or(usize::MAX)
            .max(1);
        while self.checkpoints.len() > keep {
            self.checkpoints.pop_front();
        }
    }

    /// Undo everything above `fork_at`: back to the newest checkpoint at or
    /// below it, or to nothing read when none is.
    fn restore(&mut self, fork_at: u64) {
        self.checkpoints
            .retain(|checkpoint| checkpoint.number <= fork_at);
        match self.checkpoints.back() {
            Some(checkpoint) => {
                tracing::info!(chain = %self.vault_chain, from = checkpoint.number, "the record goes back to a checkpoint below the replaced blocks");
                self.state = checkpoint.state.clone();
                self.head = None;
            }
            None => self.reset(),
        }
    }

    /// Everything read again from the deployment.
    fn reset(&mut self) {
        self.state = Derived {
            history: History::default(),
            facts: Facts::new(),
            facts_bytes: 0,
            readers: Readers::new(&self.deployments, &self.policy),
        };
        self.checkpoints.clear();
        self.head = None;
    }
}

/// What the record's build came to.
struct Built {
    strategies: Vec<Strategy>,
    authors: Option<Vec<(Address, Author)>>,
    feed: Vec<shared::model::registry::FeedEvent>,
    trades: u32,
    terms: Option<shared::Terms>,
    payments: Option<u64>,
}

impl Built {
    #[allow(clippy::too_many_arguments)]
    fn of(
        views: &Views,
        vault_chain: Chain,
        deployments: &Deployments,
        store: &Store,
        history: &mut History,
        health: &HashMap<Address, Health>,
        now: u64,
        read_through: bool,
        past: &PastReads,
    ) -> Result<Built> {
        let strategies = read::strategies(
            views,
            vault_chain,
            deployments,
            store,
            history,
            health,
            now,
            read_through,
            past,
        )?;
        // Authors that can't be read leave the authors as last read, and
        // hold up nothing else of the chain's.
        let authors = match read::authors(views, deployments) {
            Ok(authors) => Some(authors),
            Err(error) => {
                tracing::warn!(chain = %vault_chain, %error, "could not read the authors; kept them as last read");
                None
            }
        };
        let feed = read::feed(vault_chain, history, &strategies);
        let trades = strategies
            .iter()
            .map(|strategy| read::trade_count(history, strategy))
            .sum();
        let terms = read::terms(views, deployments)
            .inspect_err(|error| tracing::debug!(chain = %vault_chain, %error, "the terms could not be read; kept as last read"))
            .ok();
        let payments = read::payments(views, deployments).ok();
        Ok(Built {
            strategies,
            authors,
            feed,
            trades,
            terms,
            payments,
        })
    }
}

/// The answers of an observation, by read.
fn answers_of(plan: &Plan, observed: &chain::follow::Observed) -> HashMap<Key, Item> {
    plan.reads
        .iter()
        .zip(observed.items.iter())
        .map(|(read, item)| ((read.target, read.data.clone()), item.clone()))
        .collect()
}

/// The supply of `vault` just before the payment `paid`, as it was at the
/// block before: that block's header is read and checked to be the parent
/// of the block the payment was logged in, so the read is of the branch the
/// log was checked on.
async fn supply_before(past: &mut Follower, vault: Address, paid: read::Paid) -> Result<U256> {
    let at = past
        .header_any(BlockNumberOrTag::Number(paid.block))
        .await?;
    if at.hash != paid.block_hash {
        bail!(
            "block {} is no longer the block the payment was logged in",
            paid.block
        );
    }
    let before = past
        .header_any(BlockNumberOrTag::Number(paid.block.saturating_sub(1)))
        .await?;
    if before.hash != at.parent_hash {
        bail!("block {}'s header is not its child's parent", before.number);
    }
    let mut plan = Plan::default();
    plan.read(vault, &IVault::totalSupplyCall {});
    let observed = past.observe(&plan, At::Pinned(before)).await?;
    observed.decode::<IVault::totalSupplyCall>(0)
}

/// Reads at past blocks for the readings, through the past follower.
struct Sampler<'a> {
    follower: &'a mut Follower,
}

impl read::Past for Sampler<'_> {
    async fn header(&mut self, number: u64) -> Result<Header> {
        self.follower
            .header_any(BlockNumberOrTag::Number(number))
            .await
    }

    async fn prices(&mut self, vault: Address, block: Header) -> Result<(f64, f64)> {
        let plan = read::price_reads(vault);
        let observed = self.follower.observe(&plan, At::Pinned(block)).await?;
        read::prices_from(&observed)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A deployment whose contracts are at addresses one apart.
    pub(crate) fn deployments() -> Deployments {
        let address = |byte: u8| format!("0x{}", format!("{byte:02x}").repeat(20));
        let mut fields = vec![
            r#""chain_key":"evm:31337""#.to_string(),
            r#""chain_id":31337"#.to_string(),
            r#""rpc_url":"""#.to_string(),
            r#""deployed_block":0"#.to_string(),
        ];
        for (index, name) in [
            "usd",
            "venue",
            "attestation_verifier",
            "attestation",
            "publishing",
            "prices",
            "strategies",
            "reviews",
            "profiles",
            "factory",
            "station_factory",
            "driver",
            "bridge",
        ]
        .iter()
        .enumerate()
        {
            fields.push(format!(r#""{name}":"{}""#, address(0xa0 + index as u8)));
        }
        let roles: Vec<String> = [
            "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper",
            "operator", "author", "viewer",
        ]
        .iter()
        .map(|role| format!(r#""{role}":"{}""#, address(0x01)))
        .collect();
        fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
        Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
    }
}
