//! Where the keeper's reads come from.
//!
//! The decision code (`keep_one` and everything it calls) reads the chain
//! through [`Observed`], which looks like the chain's [`Client`] (it derefs
//! to it) and answers the same reads. Built from a client alone it reads the
//! chain itself, every read at the head, as the keeper always has: that is
//! the active path. Built for a wake from an observation (one consistent
//! read of every vault on the chain, at one block) it answers from that
//! observation instead:
//!
//! - a view the keeper has read before on this chain is in the observation,
//!   and is answered from it, at the observation's block;
//! - a fact a vault or station never changes once made (a vault's timers,
//!   home station, author and creation time, a station's spec and pinned
//!   drivers, a driver's exit path and name) is answered from what an
//!   earlier observation read, and read again once a replacement of the
//!   branch below the block it was read at is proved;
//! - the chain's time and head are the observation's;
//! - anything else (a simulation of a send, a view never read before, a
//!   view the observation answered with a revert that says nothing, or with
//!   something that doesn't decode, a log outside what the chain's log reader has checked)
//!   is read from the chain itself, as before, and a view so read is in the
//!   next observation.
//!
//! Once anything is sent, every read for the rest of the wake is made of
//! the chain itself: what the send changed is never answered from the
//! observation taken before it.
//!
//! What the keeper reads on a chain is learned from its reads ([`Learned`]),
//! so the next observation holds exactly what the decision code asks for,
//! and nothing a hand-written list could leave out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log, TransactionReceipt, TransactionRequest};
use alloy::sol_types::SolCall;
use anyhow::{Context, Result, anyhow};
use chain::Client;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::driver::{IDelayedDriver, IDriver};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::IVault;
use chain::follow::{self, Item, Plan};

alloy::sol! {
    /// What a station was made by, and what that factory names as the
    /// registry its keys are blessed in. Not in the station's shared
    /// interface.
    interface IMadeBy {
        function factory() external view returns (address);
        function attestation() external view returns (address);
    }
}

/// How long a read the keeper no longer makes stays in its observations,
/// unless the chain's loop sets another (see [`Learned::keep_reads_for`]): a
/// read made once a day (a fill's) is read of the chain itself that once.
pub const KEEP_READS: Duration = Duration::from_secs(10 * 60);

/// How long after an observation its block's time is still taken as the
/// chain's: longer than a wake that reads nothing of the chain itself
/// takes; a wake slowed by its own reads of the chain reads the time again.
const TIME_FRESH: Duration = Duration::from_millis(500);

/// What a read is to an observation: a view whose answer may change from one
/// block to the next, or a fact its contract never changes once made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Changing,
    Fixed,
}

/// Whether a call is a view the keeper reads through observations, and of
/// which kind. Anything else (a simulation of a send, a view not listed) is
/// read of the chain itself.
pub(crate) fn kind_of(data: &[u8]) -> Option<Kind> {
    let selector: [u8; 4] = data.get(..4)?.try_into().ok()?;
    let fixed: &[[u8; 4]] = &[
        IVault::timersCall::SELECTOR,
        IVault::homeStationCall::SELECTOR,
        IVault::createdAtCall::SELECTOR,
        IVault::authorCall::SELECTOR,
        IStationVault::haltGraceCall::SELECTOR,
        IStation::specCall::SELECTOR,
        IStation::pinnedDriverCall::SELECTOR,
        IDriver::exitPathCall::SELECTOR,
        IMadeBy::factoryCall::SELECTOR,
        IMadeBy::attestationCall::SELECTOR,
    ];
    if fixed.contains(&selector) {
        return Some(Kind::Fixed);
    }
    let changing: &[[u8; 4]] = &[
        IVault::stateCall::SELECTOR,
        IVault::tankCall::SELECTOR,
        IVault::balanceOfCall::SELECTOR,
        IVault::cashCall::SELECTOR,
        IVault::claimCountCall::SELECTOR,
        IVault::claimAtCall::SELECTOR,
        IVault::lastPaymentCall::SELECTOR,
        IVault::lastPricedCall::SELECTOR,
        IVault::needsValuationFirstCall::SELECTOR,
        IVault::nextUnpricedCall::SELECTOR,
        IVault::closeWantsValuationCall::SELECTOR,
        IVault::requestCountCall::SELECTOR,
        IVault::requestAtCall::SELECTOR,
        IVault::unitsCall::SELECTOR,
        IVault::unitsOfCall::SELECTOR,
        IVault::unitsSinceCall::SELECTOR,
        IVault::valuationAtCall::SELECTOR,
        IVault::valuationCountCall::SELECTOR,
        IVault::bundleHashCall::SELECTOR,
        IVault::restrictedCall::SELECTOR,
        IStationVault::haltedAtCall::SELECTOR,
        IStationVault::unsettledCall::SELECTOR,
        IStation::approvedKeyCall::SELECTOR,
        IStation::outsideBandCall::SELECTOR,
        IStation::stageCall::SELECTOR,
        IStation::stoppedCall::SELECTOR,
        IStation::valueCall::SELECTOR,
        IStation::stoppedBandCall::SELECTOR,
        IStation::reclaimedCall::SELECTOR,
        IVaultFactory::vaultsCall::SELECTOR,
        crate::IReplacesFactory::previousCall::SELECTOR,
        IDelayedDriver::movableAtCall::SELECTOR,
        IDelayedDriver::exposureCall::SELECTOR,
        IAttestationRegistry::blessingTtlCall::SELECTOR,
        IAttestationRegistry::blessedUntilCall::SELECTOR,
        IAttestationRegistry::isBlessedForCall::SELECTOR,
        IAttestationRegistry::keyInfoCall::SELECTOR,
        IAttestationRegistry::isBlessedAsCall::SELECTOR,
        IAttestationRegistry::isDriverRevokedCall::SELECTOR,
        IAttestationRegistry::rootKeyCall::SELECTOR,
        IAttestationRegistry::isVerifierAllowedCall::SELECTOR,
        IAttestationRegistry::isImageAllowedCall::SELECTOR,
        IAttestationRegistry::isProjectAllowedCall::SELECTOR,
        IAttestationRegistry::verifiersCall::SELECTOR,
    ];
    changing.contains(&selector).then_some(Kind::Changing)
}

/// One read, as the observation keys it: the contract and the calldata.
type Key = (Address, Bytes);

/// One observation, as reads are answered from it.
#[derive(Debug)]
pub struct Snapshot {
    pub number: u64,
    pub timestamp: u64,
    pub parent_hash: B256,
    /// The block's own hash, when the observation knows it.
    pub hash: Option<B256>,
    /// When it was taken, on the keeper's clock.
    pub taken: Instant,
    reads: HashMap<Key, Item>,
    balances: HashMap<Address, U256>,
}

impl Snapshot {
    /// The answers of `observed`, which read `plan`.
    pub fn new(plan: &Plan, observed: &follow::Observed) -> Self {
        let reads = plan
            .reads
            .iter()
            .zip(observed.items.iter())
            .map(|(read, item)| ((read.target, read.data.clone()), item.clone()))
            .collect();
        let balances = plan
            .balances
            .iter()
            .copied()
            .zip(observed.balances.iter().copied())
            .collect();
        Self {
            number: observed.number,
            timestamp: observed.timestamp,
            parent_hash: observed.parent_hash,
            hash: observed.hash,
            taken: Instant::now(),
            reads,
            balances,
        }
    }

    /// Every read whose answer differs from `before`'s, among those of the
    /// selectors given: what changed between two observations.
    pub fn changed_since(&self, before: &Snapshot, selectors: &[[u8; 4]]) -> Vec<Address> {
        let mut changed = Vec::new();
        for ((target, data), item) in &self.reads {
            let Some(selector) = data.get(..4) else {
                continue;
            };
            if !selectors.iter().any(|wanted| wanted.as_slice() == selector) {
                continue;
            }
            if before
                .reads
                .get(&(*target, data.clone()))
                .is_some_and(|earlier| earlier != item)
                && !changed.contains(target)
            {
                changed.push(*target);
            }
        }
        changed
    }
}

/// What the keeper reads on one chain, learned from its reads, shared by
/// the chain's wakes and its key loops.
#[derive(Default)]
pub struct Learned {
    /// Each changing view read, and when it was last read.
    reads: HashMap<Key, Instant>,
    /// Each native balance read, and when it was last read.
    balances: HashMap<Address, Instant>,
    /// Each fixed fact, and the block whose observation it was read in.
    fixed: HashMap<Key, (Bytes, u64)>,
    /// Fixed facts read of the chain itself, waiting for an observation to
    /// read them at a known block.
    fixed_wanted: HashSet<Key>,
    /// The newest observation served, for the key loops.
    latest: Option<Arc<Snapshot>>,
    /// Block timestamps seen, by number, with the block's hash.
    headers: BTreeMap<u64, (B256, u64)>,
    /// The last block interval measured on this chain, in seconds.
    pub block_interval: Option<u64>,
    /// How long a read stays in observations once it is no longer made;
    /// [`KEEP_READS`] until set.
    keep: Option<Duration>,
}

/// [`Learned`], shared.
pub type Shared = Arc<Mutex<Learned>>;

/// The most block timestamps remembered.
const HEADERS_KEPT: usize = 64;

impl Learned {
    /// The plan for the next observation: every changing view read within
    /// [`KEEP_READS`], every fixed fact not yet read in an observation, and
    /// every balance read within it (at most 256).
    pub fn plan(&mut self, now: Instant) -> Plan {
        let keep = self.keep.unwrap_or(KEEP_READS);
        self.reads
            .retain(|_, used| now.saturating_duration_since(*used) < keep);
        self.balances
            .retain(|_, used| now.saturating_duration_since(*used) < keep);
        let mut keys: Vec<&Key> = self.reads.keys().chain(self.fixed_wanted.iter()).collect();
        keys.sort();
        let mut plan = Plan::default();
        for (target, data) in keys {
            plan.reads.push(follow::Read {
                target: *target,
                data: data.clone(),
            });
        }
        let mut balances: Vec<Address> = self.balances.keys().copied().collect();
        balances.sort();
        balances.truncate(chain::follow::view_reads::MAX_BALANCES);
        plan.balances = balances;
        plan
    }

    /// Keep a read in observations for `keep` after it was last made: long
    /// enough to span the looks between two of the reader's own (a key
    /// loop's, a slot's), short enough that a read with a moment in it (a
    /// slot's time) leaves soon after its moment.
    pub fn keep_reads_for(&mut self, keep: Duration) {
        self.keep = Some(keep);
    }

    /// Take what an observation read of fixed facts, and remember it as
    /// the newest observation.
    pub fn observed(&mut self, snapshot: &Arc<Snapshot>) {
        for key in std::mem::take(&mut self.fixed_wanted) {
            match snapshot.reads.get(&key) {
                Some(Item::Returned(answer)) => {
                    self.fixed.insert(key, (answer.clone(), snapshot.number));
                }
                _ => {
                    self.fixed_wanted.insert(key);
                }
            }
        }
        if let Some(hash) = snapshot.hash {
            self.header(snapshot.number, hash, snapshot.timestamp);
        }
        self.latest = Some(snapshot.clone());
    }

    /// No observation is current: the key loops read the chain itself.
    pub fn unserved(&mut self) {
        self.latest = None;
    }

    /// The newest observation served, taken no earlier than `after`.
    pub fn latest_after(&self, after: Option<Instant>) -> Option<Arc<Snapshot>> {
        self.latest
            .clone()
            .filter(|latest| after.is_none_or(|after| latest.taken > after))
    }

    /// A proved replacement of every block above `fork_at`: fixed facts read
    /// above it are read again, as are remembered block timestamps.
    pub fn forget_above(&mut self, fork_at: u64) {
        let forgotten: Vec<Key> = self
            .fixed
            .iter()
            .filter(|(_, (_, at))| *at > fork_at)
            .map(|(key, _)| key.clone())
            .collect();
        for key in forgotten {
            self.fixed.remove(&key);
            self.fixed_wanted.insert(key);
        }
        self.headers.retain(|number, _| *number <= fork_at);
        self.latest = None;
    }

    /// A replacement deeper than the anchors reach: everything read of the
    /// chain is read again.
    pub fn forget_all(&mut self) {
        self.forget_above(0);
        self.headers.clear();
    }

    fn header(&mut self, number: u64, hash: B256, timestamp: u64) {
        self.headers.insert(number, (hash, timestamp));
        while self.headers.len() > HEADERS_KEPT {
            self.headers.pop_first();
        }
    }

    /// The timestamp of block `number` whose hash is `hash`, if seen.
    fn timestamp_of(&self, number: u64, hash: B256) -> Option<u64> {
        self.headers
            .get(&number)
            .filter(|(seen, _)| *seen == hash)
            .map(|(_, timestamp)| *timestamp)
    }
}

/// What a wake answers its reads from.
struct Wake {
    snapshot: Arc<Snapshot>,
    /// Set once anything is sent: every read after it is the chain's own.
    sent: AtomicBool,
    /// Views read of the chain itself during the wake, before any send.
    memo: Mutex<HashMap<Key, Bytes>>,
    /// The logs the chain's reader has checked, and how far.
    logs: Option<Arc<Logs>>,
}

/// The logs the chain's log reader has read and checked.
#[derive(Debug, Default)]
pub struct Logs {
    /// Every watched log through this block has been read and checked.
    pub through: u64,
    /// The first block read for each watched address.
    pub from: HashMap<Address, u64>,
    /// The watched first topics.
    pub topics: Vec<B256>,
    /// Every log read, in block order.
    pub logs: Vec<Log>,
}

impl Logs {
    /// The logs `filter` asks for, if every block it asks about has been
    /// read for every address it names.
    fn answer(&self, filter: &Filter) -> Option<Vec<Log>> {
        let from = filter.get_from_block()?;
        let to = filter.get_to_block()?;
        if to > self.through || from > to {
            return None;
        }
        let addresses: Vec<Address> = filter.address.iter().copied().collect();
        if addresses.is_empty() {
            return None;
        }
        for address in &addresses {
            if self.from.get(address).is_none_or(|first| *first > from) {
                return None;
            }
        }
        let topics: Vec<B256> = filter.topics[0].iter().copied().collect();
        if topics.is_empty() || topics.iter().any(|topic| !self.topics.contains(topic)) {
            return None;
        }
        Some(
            self.logs
                .iter()
                .filter(|log| {
                    log.block_number
                        .is_some_and(|number| number >= from && number <= to)
                        && addresses.contains(&log.address())
                        && log
                            .topics()
                            .first()
                            .is_some_and(|topic| topics.contains(topic))
                })
                .cloned()
                .collect(),
        )
    }
}

/// The keeper's reads of one chain: the chain itself, or a wake's
/// observation (see the module's documentation).
#[derive(Clone)]
pub struct Observed {
    client: Client,
    learned: Option<Shared>,
    wake: Option<Arc<Wake>>,
    /// Sends made through this, in all.
    sends: Arc<AtomicU64>,
}

impl std::ops::Deref for Observed {
    type Target = Client;

    fn deref(&self) -> &Client {
        &self.client
    }
}

impl From<Client> for Observed {
    /// Reads of the chain itself, learning nothing.
    fn from(client: Client) -> Self {
        Self {
            client,
            learned: None,
            wake: None,
            sends: Default::default(),
        }
    }
}

impl Observed {
    /// Reads of the chain itself, every view read noted in `learned` for the
    /// next observation: the active path.
    pub fn live(client: Client, learned: Shared) -> Self {
        Self {
            client,
            learned: Some(learned),
            wake: None,
            sends: Default::default(),
        }
    }

    /// Reads answered from `snapshot` (see the module's documentation),
    /// with logs from `logs` where they cover the read.
    pub fn served(
        client: Client,
        learned: Shared,
        snapshot: Arc<Snapshot>,
        logs: Option<Arc<Logs>>,
    ) -> Self {
        Self {
            client,
            learned: Some(learned),
            wake: Some(Arc::new(Wake {
                snapshot,
                sent: AtomicBool::new(false),
                memo: Default::default(),
                logs,
            })),
            sends: Default::default(),
        }
    }

    /// The observation reads are answered from, while nothing has been sent.
    pub fn snapshot(&self) -> Option<&Arc<Snapshot>> {
        self.wake
            .as_ref()
            .filter(|wake| !wake.sent.load(Ordering::SeqCst))
            .map(|wake| &wake.snapshot)
    }

    /// The chain's block interval as last measured, in seconds.
    pub fn block_interval_seen(&self) -> Option<u64> {
        self.learned().and_then(|learned| learned.block_interval)
    }

    /// How many sends were made through this.
    pub fn sends(&self) -> u64 {
        self.sends.load(Ordering::SeqCst)
    }

    fn learned(&self) -> Option<std::sync::MutexGuard<'_, Learned>> {
        self.learned
            .as_ref()
            .and_then(|learned| learned.lock().ok())
    }

    /// Note a read for the next observation, and say what kind it is.
    fn note(&self, to: Address, data: &Bytes) -> Option<Kind> {
        let kind = kind_of(data)?;
        if let Some(mut learned) = self.learned() {
            match kind {
                Kind::Changing => {
                    learned.reads.insert((to, data.clone()), Instant::now());
                }
                Kind::Fixed => {
                    if !learned.fixed.contains_key(&(to, data.clone())) {
                        learned.fixed_wanted.insert((to, data.clone()));
                    }
                }
            }
        }
        Some(kind)
    }

    /// The raw answer to a view, from the observation, a fixed fact, or this
    /// wake's own earlier read of the chain, without reading anything.
    fn answered(&self, to: Address, data: &Bytes, kind: Kind) -> Option<Result<Bytes, Bytes>> {
        let wake = self.wake.as_ref()?;
        // A fixed fact read at a proved block is the same after a send.
        if kind == Kind::Fixed
            && let Some(learned) = self.learned()
            && let Some((answer, _)) = learned.fixed.get(&(to, data.clone()))
        {
            return Some(Ok(answer.clone()));
        }
        if wake.sent.load(Ordering::SeqCst) {
            return None;
        }
        match wake.snapshot.reads.get(&(to, data.clone())) {
            Some(Item::Returned(answer)) => return Some(Ok(answer.clone())),
            Some(Item::Reverted(reason)) => return Some(Err(reason.clone())),
            None => {}
        }
        wake.memo
            .lock()
            .ok()
            .and_then(|memo| memo.get(&(to, data.clone())).cloned())
            .map(Ok)
    }

    /// Read a contract, without sending anything: from the observation where
    /// it can be answered there, otherwise of the chain itself.
    pub async fn read<C: SolCall>(&self, to: Address, call: C) -> Result<C::Return> {
        let data = Bytes::from(call.abi_encode());
        let kind = self.note(to, &data);
        if let Some(kind) = kind
            && let Some(answer) = self.answered(to, &data, kind)
        {
            match answer {
                Ok(answer) => {
                    // An answer that doesn't decode is read again of the
                    // chain itself, as it was before observations.
                    if let Ok(decoded) = C::abi_decode_returns(&answer) {
                        return Ok(decoded);
                    }
                }
                // A revert that says why is the read's own answer at the
                // observation's block. One with nothing to say may be a call
                // inside the read starved of the gas the batch gives each
                // read: it is read again of the chain itself, with the
                // node's own gas, as it was before observations.
                Err(reason) if !reason.is_empty() => {
                    return Err(anyhow::Error::new(reverted(&reason)))
                        .with_context(|| format!("reading {} at {to}", C::SIGNATURE));
                }
                Err(_) => {}
            }
        }
        let returned = self.call_raw(to, &data).await.map_err(|error| {
            anyhow::Error::new(error).context(format!("reading {} at {to}", C::SIGNATURE))
        })?;
        if kind.is_some()
            && let Some(wake) = &self.wake
            && !wake.sent.load(Ordering::SeqCst)
            && let Ok(mut memo) = wake.memo.lock()
        {
            memo.insert((to, data.clone()), returned.clone());
        }
        C::abi_decode_returns(&returned)
            .with_context(|| format!("decoding what {} returned", C::SIGNATURE))
    }

    /// The answer to a view already in hand (the observation, a fixed fact,
    /// or this wake's own read), without reading anything: `None` when the
    /// wake has not read it. For planning the next wake.
    pub fn peek<C: SolCall>(&self, to: Address, call: C) -> Option<Result<C::Return>> {
        let data = Bytes::from(call.abi_encode());
        let kind = kind_of(&data)?;
        Some(match self.answered(to, &data, kind)? {
            Ok(answer) => C::abi_decode_returns(&answer).map_err(anyhow::Error::from),
            Err(reason) => Err(anyhow!("{} reverted ({reason})", C::SIGNATURE)),
        })
    }

    async fn call_raw(
        &self,
        to: Address,
        data: &Bytes,
    ) -> Result<Bytes, alloy::transports::TransportError> {
        let request = TransactionRequest::default()
            .with_to(to)
            .with_input(data.clone());
        self.client.provider().call(request).await
    }

    /// The observation, while it is fresh enough to tell the time by: a
    /// wake that has taken longer than [`TIME_FRESH`] (simulations of its
    /// own, say) reads the head's time again, as the keeper always has
    /// between a round's steps, so a window is never judged on a clock that
    /// has moved on.
    fn clock(&self) -> Option<&Arc<Snapshot>> {
        self.snapshot()
            .filter(|snapshot| snapshot.taken.elapsed() < TIME_FRESH)
    }

    /// The chain's time: the observation's, or the head's.
    pub async fn block_timestamp(&self) -> Result<u64> {
        match self.clock() {
            Some(snapshot) => Ok(snapshot.timestamp),
            None => self.client.block_timestamp().await,
        }
    }

    /// The head's number: the observation's, or the head's. No decision
    /// reads it today; it is here so one that does reads the observation's
    /// head rather than the chain's through the client underneath.
    #[allow(dead_code)]
    pub async fn block_number(&self) -> Result<u64> {
        match self.clock() {
            Some(snapshot) => Ok(snapshot.number),
            None => self.client.block_number().await,
        }
    }

    /// The block logs are read up to: on the active path the head, as
    /// before; from an observation, the last block the chain's log reader
    /// has read and checked, so a log read never goes past what it can be
    /// answered from (see [`Observed::logs`]). A send in the wake changes
    /// nothing of that: what it logged is read and checked at a later look,
    /// never taken unchecked meanwhile.
    pub async fn log_head(&self) -> Result<u64> {
        if let Some(logs) = self.wake.as_ref().and_then(|wake| wake.logs.as_ref()) {
            return Ok(logs.through);
        }
        self.client.block_number().await
    }

    /// The logs a filter matches: from the chain's log reader where it has
    /// read and checked every block and address asked about, otherwise of
    /// the chain itself.
    pub async fn logs(&self, filter: &Filter) -> Result<Vec<Log>> {
        if let Some(logs) = self.wake.as_ref().and_then(|wake| wake.logs.as_ref())
            && let Some(found) = logs.answer(filter)
        {
            tracing::debug!(
                from = ?filter.get_from_block(),
                to = ?filter.get_to_block(),
                found = found.len(),
                "logs answered from the checked reader"
            );
            return Ok(found);
        }
        self.client.logs(filter).await
    }

    /// A past block's time: the observation's own block's from the
    /// observation, any other read.
    pub async fn block_timestamp_at(&self, block: u64) -> Result<u64> {
        match self.clock() {
            Some(snapshot) if block == snapshot.number => Ok(snapshot.timestamp),
            _ => self.client.block_timestamp_at(block).await,
        }
    }

    /// How far apart the chain's last two blocks are, in seconds, at least
    /// one: the head's time less its parent's. The head (its number, time
    /// and parent's hash) comes from one answer: the observation, or one
    /// read of the head; the parent is the block with that hash, remembered
    /// from an earlier look or read by number and checked to carry it. So
    /// the interval never comes from two reads that may have reached nodes
    /// at different heads (a node a block behind has the parent). When the
    /// parent can't be read, the interval this chain last showed is taken,
    /// so the vault's steps aren't skipped for it.
    pub async fn block_interval(&self) -> Result<u64> {
        let head = match self.clock() {
            Some(snapshot) => Ok((snapshot.number, snapshot.timestamp, snapshot.parent_hash)),
            None => self
                .client
                .provider()
                .get_block(alloy::eips::BlockId::latest())
                .await
                .context("the head block")
                .and_then(|block| block.ok_or_else(|| anyhow!("the chain has no blocks yet")))
                .map(|block| {
                    (
                        block.header.number,
                        block.header.timestamp,
                        block.header.parent_hash,
                    )
                }),
        };
        let measured = match head {
            Ok((number, timestamp, parent_hash)) => self
                .parent_timestamp(number, parent_hash)
                .await
                .map(|parent| timestamp.saturating_sub(parent).max(1)),
            Err(error) => Err(error),
        };
        match measured {
            Ok(interval) => {
                self.measured(interval);
                Ok(interval)
            }
            Err(error) => match self.block_interval_seen() {
                Some(interval) => {
                    tracing::debug!(
                        error = format!("{error:#}"),
                        interval,
                        "the block interval could not be measured; the last measured is used"
                    );
                    Ok(interval)
                }
                None => Err(error),
            },
        }
    }

    /// The time of the block before `number`, whose hash is `hash`.
    async fn parent_timestamp(&self, number: u64, hash: B256) -> Result<u64> {
        let parent = number
            .checked_sub(1)
            .ok_or_else(|| anyhow!("the head has no parent"))?;
        if let Some(known) = self
            .learned()
            .and_then(|learned| learned.timestamp_of(parent, hash))
        {
            return Ok(known);
        }
        let header = self
            .client
            .provider()
            .get_block(alloy::eips::BlockId::number(parent))
            .await
            .context("a past block")?
            .ok_or_else(|| anyhow!("no such block"))?;
        anyhow::ensure!(
            header.header.hash == hash,
            "block {parent} as read is not the head's parent"
        );
        if let Some(mut learned) = self.learned() {
            learned.header(parent, hash, header.header.timestamp);
        }
        Ok(header.header.timestamp)
    }

    fn measured(&self, interval: u64) {
        if let Some(mut learned) = self.learned() {
            learned.block_interval = Some(interval);
        }
    }

    /// A native balance: the observation's, or the head's.
    pub async fn balance(&self, of: Address) -> Result<U256> {
        if let Some(mut learned) = self.learned() {
            learned.balances.insert(of, Instant::now());
        }
        if let Some(snapshot) = self.snapshot()
            && let Some(balance) = snapshot.balances.get(&of)
        {
            return Ok(*balance);
        }
        self.client.balance(of).await
    }

    /// Everything read after a send that may have gone out is read of the
    /// chain itself: one the node refused at its estimate, or one that was
    /// never handed to it, changed nothing, and anything else may have.
    fn sent<T>(&self, sent: Result<T>) -> Result<T> {
        let refused = sent.as_ref().is_err_and(|error| {
            (crate::execution_reverted(error)
                || error.chain().any(|cause| cause.is::<chain::NeverSent>()))
                && !error.chain().any(|cause| cause.is::<chain::Reverted>())
        });
        if !refused {
            self.sends.fetch_add(1, Ordering::SeqCst);
            if let Some(wake) = &self.wake {
                wake.sent.store(true, Ordering::SeqCst);
            }
            if let Some(mut learned) = self.learned() {
                // The key loops read the chain itself until an observation
                // taken after this send.
                learned.unserved();
            }
        }
        sent
    }

    /// Send a call as `from` with the gas its estimate gives, and wait for
    /// it to land. The estimate is asked first whoever signs (see
    /// [`Client::send_estimated`]): a call it refuses is not sent, so none
    /// the chain would revert is paid for, the block's whole gas limit with
    /// it on a node that signs.
    pub async fn send<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
    ) -> Result<TransactionReceipt> {
        self.sent(self.client.send_estimated(from, to, call).await)
    }

    /// Send a call as `from` with the gas limit given, and wait for it to
    /// land.
    pub async fn send_with_gas_limit<C: SolCall>(
        &self,
        from: Address,
        to: Address,
        call: C,
        gas_limit: u64,
    ) -> Result<TransactionReceipt> {
        self.sent(
            self.client
                .send_with_gas_limit(from, to, call, gas_limit)
                .await,
        )
    }

    /// Send coin, noting the transaction's hash once it is signed.
    pub async fn transfer_noting(
        &self,
        from: Address,
        to: Address,
        value: U256,
        sent_as: impl FnOnce(B256) + Send,
    ) -> Result<TransactionReceipt> {
        self.sent(self.client.transfer_noting(from, to, value, sent_as).await)
    }
}

/// A revert, as a node answers a call that reverts with `reason`.
fn reverted(reason: &Bytes) -> alloy::transports::TransportError {
    let data = serde_json::value::to_raw_value(&reason.to_string()).ok();
    alloy::transports::TransportError::ErrorResp(alloy::rpc::json_rpc::ErrorPayload {
        code: 3,
        message: "execution reverted".into(),
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revert_from_an_observation_reads_as_the_node_s_revert() {
        let error = anyhow::Error::new(reverted(&Bytes::from(vec![0xde, 0xad, 0xbe, 0xef])));
        assert!(crate::execution_reverted(&error));
        assert!(crate::reverted_with(
            &error,
            [0xde, 0xad, 0xbe, 0xef],
            "Nothing()"
        ));
    }

    #[test]
    fn a_send_simulated_through_a_read_is_never_answered_from_an_observation() {
        for simulation in [
            IVault::closeCall {}.abi_encode(),
            IVault::recordValuationCall {}.abi_encode(),
            IStation::unwindCall {
                fraction: U256::from(1u64),
            }
            .abi_encode(),
            IStation::settleCloseCall {}.abi_encode(),
            IStation::revokeKeyCall {
                driver: B256::ZERO,
                key: Address::ZERO,
            }
            .abi_encode(),
        ] {
            assert_eq!(kind_of(&simulation), None);
        }
    }

    #[test]
    fn facts_a_vault_never_changes_are_fixed_and_its_state_is_not() {
        assert_eq!(
            kind_of(&IVault::timersCall {}.abi_encode()),
            Some(Kind::Fixed)
        );
        assert_eq!(
            kind_of(&IVault::homeStationCall {}.abi_encode()),
            Some(Kind::Fixed)
        );
        assert_eq!(
            kind_of(&IVault::stateCall {}.abi_encode()),
            Some(Kind::Changing)
        );
        assert_eq!(
            kind_of(&IStation::approvedKeyCall { driver: B256::ZERO }.abi_encode()),
            Some(Kind::Changing)
        );
    }

    #[test]
    fn a_read_unused_for_longer_than_it_is_kept_leaves_the_plan() {
        let mut learned = Learned::default();
        let now = Instant::now();
        let vault = Address::repeat_byte(1);
        learned.reads.insert(
            (vault, IVault::stateCall {}.abi_encode().into()),
            now - KEEP_READS - Duration::from_secs(1),
        );
        learned
            .reads
            .insert((vault, IVault::tankCall {}.abi_encode().into()), now);
        let plan = learned.plan(now);
        assert_eq!(plan.reads.len(), 1);
        assert_eq!(
            plan.reads[0].data,
            Bytes::from(IVault::tankCall {}.abi_encode())
        );
    }

    #[test]
    fn fixed_facts_read_above_a_proved_fork_are_read_again() {
        let mut learned = Learned::default();
        let vault = Address::repeat_byte(1);
        let below = (vault, Bytes::from(IVault::timersCall {}.abi_encode()));
        let above = (
            Address::repeat_byte(2),
            Bytes::from(IVault::timersCall {}.abi_encode()),
        );
        learned.fixed.insert(below.clone(), (Bytes::new(), 10));
        learned.fixed.insert(above.clone(), (Bytes::new(), 30));
        learned.forget_above(20);
        assert!(learned.fixed.contains_key(&below));
        assert!(!learned.fixed.contains_key(&above));
        assert!(learned.fixed_wanted.contains(&above));
        let plan = learned.plan(Instant::now());
        assert_eq!(plan.reads.len(), 1);
        assert_eq!(plan.reads[0].target, above.0);
    }

    #[test]
    fn logs_are_answered_only_where_every_block_and_address_was_read() {
        let address = Address::repeat_byte(3);
        let topic = B256::repeat_byte(4);
        let mut log = Log::default();
        log.inner.address = address;
        log.inner.data = alloy::primitives::LogData::new_unchecked(vec![topic], Bytes::new());
        log.block_number = Some(15);
        let logs = Logs {
            through: 20,
            from: HashMap::from([(address, 10)]),
            topics: vec![topic],
            logs: vec![log],
        };
        let filter = |from: u64, to: u64| {
            Filter::new()
                .address(address)
                .event_signature(topic)
                .from_block(from)
                .to_block(to)
        };
        assert_eq!(
            logs.answer(&filter(10, 20)).map(|found| found.len()),
            Some(1)
        );
        assert_eq!(
            logs.answer(&filter(16, 20)).map(|found| found.len()),
            Some(0)
        );
        assert!(
            logs.answer(&filter(9, 20)).is_none(),
            "a block before the read"
        );
        assert!(
            logs.answer(&filter(10, 21)).is_none(),
            "a block not read yet"
        );
        assert!(
            logs.answer(&filter(10, 20).address(Address::repeat_byte(9)))
                .is_none(),
            "an address not watched"
        );
        assert!(
            logs.answer(&filter(10, 20).event_signature(B256::repeat_byte(8)))
                .is_none(),
            "a topic not watched"
        );
    }
}
