//! Turning the chain into what the site shows.
//!
//! Everything here is read from contracts and logs. A vault's value per share
//! is asked of the vault itself at each past block, so the record is not a
//! store anyone has to trust: delete this service's memory and it rebuilds
//! the same numbers from the chain.
//!
//! The record is built from two things the chain's reader gathers (see
//! `crate::evm`): the views of one observation, every read at one block
//! ([`Views`]), and what the chain's logs up to that block come to
//! ([`History`], folded log by log in order). Building reads nothing of the
//! chain itself; the readings taken at past blocks (each vault's hourly
//! value, the supply a paid claim was paid from) are taken apart, through
//! [`Past`].

use std::collections::{HashMap, HashSet, VecDeque};

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;
use anyhow::Result;
use chain::Deployments;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::IAuthorProfiles;
use chain::bindings::publishing::IPublishing;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::station::IStationVault;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{
    ClaimState as ChainClaimState, IVault, RequestKind as ChainRequestKind,
    VaultState as ChainVaultState,
};
use chain::bindings::venue::ILocalVenue;
use shared::model::activity::{EventKind, Kill, Side, Trade, VaultEvent};
use shared::model::attestation::{Approval, Attestation, BlessedKey, Image, KeyRole};
use shared::model::bundle::{Bundle, ModelFile};
use shared::model::common::{Chain, Point};
use shared::model::registry::{Author, FeedEvent, FeedKind, Job, JobState};
use shared::model::review::{Rating, Review};
use shared::model::strategy::{Record, Standing, Strategy};
use shared::model::vault::{
    Claim, ClaimState, Fees, Halt, PendingFloor, Request, RequestKind, Tank, Vault, VaultState,
};
use shared::model::venue::{Custody, ExitClass, ExitPath, Leg, Valuation, VenueStatus};
use time::{Date, OffsetDateTime, PrimitiveDateTime};

use crate::store::Store;
pub use crate::views::Views;

/// What the indexer remembers of a chain between refreshes: what its logs
/// have come to, the readings it has taken, and what it has learned not to
/// read again. All of it is derived from the chain as of one block, and kept
/// whole as a checkpoint at each refresh, so a reorganisation undoes it to a
/// block that is still on the chain.
#[derive(Clone, Default)]
pub struct History {
    /// Each vault's readings, oldest first: hourly for the last
    /// [`HOURLY_FOR`], daily before that.
    samples: HashMap<Address, Vec<Sample>>,
    /// The block and time of each vault's last reading: where the next one
    /// is looked for from.
    sampled: HashMap<Address, Mark>,
    /// The block of each vault's reading that the vault hasn't answered, and
    /// how many refreshes in a row it hasn't.
    unanswered: HashMap<Address, (u64, u32)>,
    /// Each vault's claims that are paid and whose payment has been read
    /// from the vault's log, by id: nothing about them changes again, so they
    /// are not read again.
    paid_claims: HashMap<Address, HashMap<u64, Claim>>,
    /// The share of the vault each claim not yet paid stood for when last
    /// read, by vault and id: what a claim seen paid since closed, if its
    /// payment can't be dated.
    waiting: HashMap<Address, HashMap<u64, f64>>,
    /// How many refreshes in a row each paid claim's supply before its
    /// payment has gone unanswered, by vault and id.
    unmeasured: HashMap<(Address, u64), u32>,
    /// The supply each paid claim was paid from, as read at the block before
    /// its payment, by vault and id.
    supplies: HashMap<(Address, u64), U256>,
    /// What is known of each vault's requests: where its waiting ones start.
    requests: HashMap<Address, Requests>,
    /// Each vault as last read whole, and the chain time it was: what is
    /// shown while a later read of it fails part-way.
    last_whole: HashMap<Address, (Strategy, u64)>,
    /// The factory's `Activated` logs: the block each vault was created in.
    activated: HashMap<Address, u64>,
    /// The registry's registrations, for the feed.
    registrations: Vec<Registration>,
    /// The images proposed for the allowlist, for the feed.
    images: Vec<(u64, B256)>,
    /// Each vault's own logs.
    vaults: HashMap<Address, VaultLogs>,
    /// Each vault's blessings.
    blessings: HashMap<Address, VecDeque<Blessing>>,
    /// Each station's fills: the vault never trades itself, its home station
    /// does.
    fills: HashMap<Address, Fills>,
}

/// A block and its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub block: u64,
    pub time: u64,
}

/// One reading of a vault, dated by its block's time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub(crate) at: u64,
    pub(crate) value_per_share: f64,
    pub(crate) aum: f64,
}

/// A registration, as the feed tells of it.
#[derive(Clone)]
struct Registration {
    at: u64,
    strategy_id: u64,
    bundle_hash: B256,
}

/// What a vault's own logs come to.
#[derive(Clone, Default)]
struct VaultLogs {
    /// The newest of what the page shows, oldest first.
    events: VecDeque<VaultEvent>,
    /// Why it last halted, and the block and transaction it halted in.
    halted: Option<B256>,
    halted_in: Option<(Option<u64>, Option<B256>)>,
    /// Where each claim was paid, by id.
    paid_in: HashMap<u64, Paid>,
    /// The newest time the vault was stopped, however many events since.
    killed: Option<VaultEvent>,
    /// What each holder has paid for what they hold.
    costs: Costs,
}

/// Where a claim's `ClaimPaid` lies: its block, that block's hash, and its
/// place in the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paid {
    pub block: u64,
    pub block_hash: B256,
    pub log_index: u64,
}

/// A blessing of a key for a vault.
#[derive(Clone)]
struct Blessing {
    signer: Address,
    until: u64,
    proof_tx: String,
}

/// A station's fills: how many, and the newest in full.
#[derive(Clone, Default)]
struct Fills {
    count: u32,
    newest: VecDeque<Trade>,
}

/// How many of a vault's events, and of its trades, the page shows.
const SHOWN: usize = 60;

/// How many refreshes in a row a vault may leave a reading unanswered before
/// it is passed over: a node that fails a read now and then loses no
/// reading, and a block the vault can't be read at holds up none after it.
const UNANSWERED_TRIES: u32 = 5;

/// How many of a vault's newest blessings are kept.
const BLESSINGS: usize = 4;

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

/// How long a vault's readings are kept hourly; older ones are kept one a
/// day, which is all the record shows.
const HOURLY_FOR: u64 = 90 * DAY;

/// The most block headers asked of the chain for one vault's readings in
/// one refresh: a vault whose readings are being rebuilt (a restart, or a
/// vault seen for the first time) takes at most this many a refresh, and
/// the rest of the record isn't held up while it catches up.
const READS_PER_REFRESH: u32 = 48;

/// A moment in chain time.
pub fn moment(timestamp: u64) -> PrimitiveDateTime {
    let when =
        OffsetDateTime::from_unix_timestamp(timestamp as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    PrimitiveDateTime::new(when.date(), when.time())
}

pub fn day(timestamp: u64) -> Date {
    moment(timestamp).date()
}

/// Dollars, as the asset counts them.
pub(crate) fn dollars(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e6
}

fn shares(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

pub(crate) fn name_of(id: B256) -> String {
    let end = id.0.iter().position(|byte| *byte == 0).unwrap_or(32);
    String::from_utf8_lossy(&id.0[..end]).to_string()
}

pub(crate) fn hash_of(value: B256) -> String {
    format!("sha256:{}", alloy::hex::encode(value))
}

pub(crate) fn address_of(value: Address) -> String {
    format!("{value:?}")
}

// The logs -------------------------------------------------------------------

/// The first topics of every log the record folds: the factory's
/// activations, the registry's registrations, the allowlist's proposals and
/// blessings, the stand-in venue's fills, and every event of a vault's the
/// page shows or a holder's cost is reckoned from.
pub fn topics() -> Vec<B256> {
    vec![
        IVaultFactory::Activated::SIGNATURE_HASH,
        IStrategyRegistry::Registered::SIGNATURE_HASH,
        IAttestationRegistry::ImageProposed::SIGNATURE_HASH,
        IAttestationRegistry::Blessed::SIGNATURE_HASH,
        ILocalVenue::Filled::SIGNATURE_HASH,
        IVault::Transfer::SIGNATURE_HASH,
        IVault::Requested::SIGNATURE_HASH,
        IVault::Deposited::SIGNATURE_HASH,
        IVault::Refunded::SIGNATURE_HASH,
        IVault::Withdrawn::SIGNATURE_HASH,
        IVault::FeesPaid::SIGNATURE_HASH,
        IVault::ClaimBooked::SIGNATURE_HASH,
        IVault::ClaimTried::SIGNATURE_HASH,
        IVault::ClaimPaid::SIGNATURE_HASH,
        IVault::ClaimPartPaid::SIGNATURE_HASH,
        IVault::ProRataTaken::SIGNATURE_HASH,
        IVault::UnitsPaid::SIGNATURE_HASH,
        IVault::TankFunded::SIGNATURE_HASH,
        IVault::TankCharged::SIGNATURE_HASH,
        IVault::TankFilled::SIGNATURE_HASH,
        IVault::StateChanged::SIGNATURE_HASH,
        IVault::Halted::SIGNATURE_HASH,
        IVault::ToStation::SIGNATURE_HASH,
        IVault::FromStation::SIGNATURE_HASH,
    ]
}

impl History {
    /// The block `vault` was created in, as the factory's log has it.
    pub fn created_in(&self, vault: &Address) -> Option<u64> {
        self.activated.get(vault).copied()
    }

    /// Every vault the factory's log has activated.
    pub fn activated(&self) -> impl Iterator<Item = (&Address, &u64)> {
        self.activated.iter()
    }

    /// What `owner` paid for what it holds of `vault`, from the vault's logs
    /// folded so far: everything put in, less everything taken out.
    pub fn cost(&self, vault: &Address, owner: &Address) -> f64 {
        self.vaults
            .get(vault)
            .map_or(0.0, |logs| logs.costs.of(owner))
    }

    /// Every holder's cost in every vault, as folded so far.
    pub fn costs(&self) -> HashMap<Address, HashMap<Address, f64>> {
        self.vaults
            .iter()
            .map(|(vault, logs)| (*vault, logs.costs.all()))
            .collect()
    }

    /// The claims whose payment is in the logs and whose supply before it
    /// has not been read: each to be read at the block before its payment.
    pub fn unmeasured_payments(&self) -> Vec<(Address, u64, Paid)> {
        let mut out = Vec::new();
        for (vault, logs) in &self.vaults {
            for (id, paid) in &logs.paid_in {
                let measured = self
                    .paid_claims
                    .get(vault)
                    .is_some_and(|claims| claims.contains_key(id))
                    || self.supplies.contains_key(&(*vault, *id));
                if !measured {
                    out.push((*vault, *id, *paid));
                }
            }
        }
        out.sort_by_key(|(vault, id, _)| (*vault, *id));
        out
    }

    /// The supply a paid claim was paid from, as read at the block before.
    pub fn measured(&mut self, vault: Address, id: u64, supply: U256) {
        self.supplies.insert((vault, id), supply);
    }

    /// Fold logs read and checked, in block and log order, into what is
    /// known of the chain. `deployments` says whose each log is.
    pub fn fold(&mut self, deployments: &Deployments, logs: &[Log]) {
        let platform = [
            deployments.factory,
            deployments.strategies,
            deployments.attestation,
            deployments.venue,
        ];
        let mut by_vault: Vec<(Address, Vec<&Log>)> = Vec::new();
        for log in logs {
            let address = log.address();
            if !platform.contains(&address) {
                match by_vault.iter_mut().find(|(vault, _)| *vault == address) {
                    Some((_, logs)) => logs.push(log),
                    None => by_vault.push((address, vec![log])),
                }
                continue;
            }
            if address == deployments.factory
                && let (Ok(activated), Some(block)) = (
                    log.log_decode::<IVaultFactory::Activated>(),
                    log.block_number,
                )
            {
                self.activated.insert(activated.inner.data.vault, block);
            }
            if address == deployments.strategies
                && let Ok(event) = log.log_decode::<IStrategyRegistry::Registered>()
            {
                self.registrations.push(Registration {
                    at: log.block_timestamp.unwrap_or_default(),
                    strategy_id: event.inner.strategyId.try_into().unwrap_or_default(),
                    bundle_hash: event.inner.data.bundleHash,
                });
            }
            if address == deployments.attestation {
                if let Ok(event) = log.log_decode::<IAttestationRegistry::ImageProposed>() {
                    self.images.push((
                        log.block_timestamp.unwrap_or_default(),
                        event.inner.imageDigest,
                    ));
                }
                if let Ok(blessed) = log.log_decode::<IAttestationRegistry::Blessed>() {
                    let made = self.blessings.entry(blessed.inner.vault).or_default();
                    made.push_back(Blessing {
                        signer: blessed.inner.signer,
                        until: blessed.inner.data.blessedUntil,
                        proof_tx: log
                            .transaction_hash
                            .map(|hash| format!("{hash:?}"))
                            .unwrap_or_default(),
                    });
                    if made.len() > BLESSINGS {
                        made.pop_front();
                    }
                }
            }
            if address == deployments.venue
                && deployments.venue != Address::ZERO
                && let Ok(filled) = log.log_decode::<ILocalVenue::Filled>()
            {
                let event = filled.inner.data;
                let made = self.fills.entry(event.master).or_default();
                made.count += 1;
                made.newest.push_back(Trade {
                    at: moment(log.block_timestamp.unwrap_or_default()),
                    venue: "Local venue".to_string(),
                    market: name_of(event.market),
                    side: if event.isBuy { Side::Buy } else { Side::Sell },
                    size: shares(event.size),
                    price: dollars(event.price),
                    notional: dollars(event.notional),
                    kind: if event.reduceOnly {
                        "reduce-only".to_string()
                    } else {
                        "taker".to_string()
                    },
                    reference: log
                        .transaction_hash
                        .map(|hash| format!("{hash:?}"))
                        .unwrap_or_default(),
                });
                if made.newest.len() > SHOWN {
                    made.newest.pop_front();
                }
            }
        }
        for (vault, logs) in by_vault {
            let made = self.vaults.entry(vault).or_default();
            for log in &logs {
                made.fold(log);
            }
            made.costs.fold(&logs);
        }
    }
}

impl VaultLogs {
    /// One of the vault's own logs: the events the page shows, and, kept as
    /// they are read, why it last halted, where each claim was paid, and
    /// when it was last stopped.
    fn fold(&mut self, log: &Log) {
        if let Ok(halted) = log.log_decode::<IVault::Halted>() {
            self.halted = Some(halted.inner.data.reason);
            self.halted_in = Some((log.block_number, log.transaction_hash));
        }
        if let (Ok(paid), Some(block), Some(block_hash), Some(log_index)) = (
            log.log_decode::<IVault::ClaimPaid>(),
            log.block_number,
            log.block_hash,
            log.log_index,
        ) && let Ok(id) = u64::try_from(paid.inner.data.claimId)
        {
            self.paid_in.insert(
                id,
                Paid {
                    block,
                    block_hash,
                    log_index,
                },
            );
        }
        // The newest sixty the feed shows, counted after the logs it doesn't
        // show (share transfers and the like) are left out.
        let event = vault_event(log).or_else(|| killed_after_halt(self, log));
        if let Some(event) = event {
            if event.kind == EventKind::Killed {
                self.killed = Some(event.clone());
            }
            self.events.push_back(event);
            if self.events.len() > SHOWN {
                self.events.pop_front();
            }
        }
    }
}

/// What each holder of a vault paid for what they still hold: everything
/// they put in, less everything they took out. The vault's own log is the
/// whole of it.
///
/// What a holder took out is found three ways, since the vault says so three
/// ways:
///
/// - a withdrawal, or a claim paid while the vault ran: `Withdrawn` under the
///   owner (and, for a claim, a `ClaimPaid` right after it);
/// - a claim paid once the vault closed (`claimClosed`): a `ClaimPaid` alone,
///   which names only the claim, so the owner is the one its `ClaimBooked`
///   names;
/// - shares redeemed once the vault closed (`redeemClosed`): a `Withdrawn`
///   under the receiver, whoever that is, so it is counted for the holder
///   whose shares were burnt for it in the same transaction, and not for the
///   receiver.
///
/// Shares never move from one holder to another (the vault refuses
/// `transfer`), so no cost moves with them.
#[derive(Clone, Default)]
struct Costs {
    paid: HashMap<Address, f64>,
    /// Who booked each claim.
    booked: HashMap<U256, Address>,
}

impl Costs {
    fn of(&self, owner: &Address) -> f64 {
        self.paid.get(owner).copied().unwrap_or(0.0).max(0.0)
    }

    fn all(&self) -> HashMap<Address, f64> {
        self.paid
            .iter()
            .map(|(owner, paid)| (*owner, paid.max(0.0)))
            .collect()
    }

    /// Fold one batch of the vault's logs (whole blocks, in order): each
    /// transaction's logs are read together, since a withdrawal is told
    /// apart by the log that follows it.
    fn fold(&mut self, logs: &[&Log]) {
        let mut start = 0;
        while start < logs.len() {
            let tx = (logs[start].block_number, logs[start].transaction_hash);
            let mut end = start + 1;
            while end < logs.len() && (logs[end].block_number, logs[end].transaction_hash) == tx {
                end += 1;
            }
            self.transaction(&logs[start..end]);
            start = end;
        }
    }

    /// One transaction's logs of the vault, in order.
    fn transaction(&mut self, logs: &[&Log]) {
        // The log at a place in the transaction's block, by log index.
        let at = |index: Option<u64>| {
            index.and_then(|index| logs.iter().find(|log| log.log_index == Some(index)))
        };
        let is_claim_paid = |log: Option<&&Log>| {
            log.is_some_and(|log| log.log_decode::<IVault::ClaimPaid>().is_ok())
        };
        // Every redemption a closed vault paid, to whomever: a `Withdrawn`
        // for no request with no claim paid after it, and its place.
        let mut redeemed_closed: Vec<(IVault::Withdrawn, Option<u64>)> = Vec::new();
        for log in logs {
            if let Ok(event) = log.log_decode::<IVault::Deposited>() {
                *self.paid.entry(event.inner.data.owner).or_default() +=
                    dollars(event.inner.data.assets);
            } else if let Ok(event) = log.log_decode::<IVault::ClaimBooked>() {
                self.booked
                    .insert(event.inner.data.claimId, event.inner.data.owner);
            } else if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
                let event = event.inner.data;
                let before_a_claim_paid = is_claim_paid(at(log.log_index.map(|i| i + 1)));
                // A closed vault's redemption names its receiver: it is
                // counted below, for whoever held the shares.
                if event.requestId.is_zero() && !before_a_claim_paid {
                    redeemed_closed.push((event, log.log_index));
                    continue;
                }
                *self.paid.entry(event.owner).or_default() -= dollars(event.assets);
            } else if let Ok(event) = log.log_decode::<IVault::ClaimPaid>() {
                // A claim paid once the vault closed, with no `Withdrawn`
                // before it: its owner's, who booked it.
                let after_a_withdrawal = log
                    .log_index
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| at(Some(i)))
                    .is_some_and(|before| before.log_decode::<IVault::Withdrawn>().is_ok());
                if !after_a_withdrawal
                    && let Some(owner) = self.booked.get(&event.inner.data.claimId).copied()
                {
                    *self.paid.entry(owner).or_default() -= dollars(event.inner.data.assets);
                }
            } else if let Ok(event) = log.log_decode::<IVault::ProRataTaken>() {
                *self.paid.entry(event.inner.data.owner).or_default() -=
                    dollars(event.inner.data.assets);
            } else if let Ok(event) = log.log_decode::<IVault::UnitsPaid>() {
                *self.paid.entry(event.inner.data.owner).or_default() -=
                    dollars(event.inner.data.assets);
            }
        }
        if redeemed_closed.is_empty() {
            return;
        }
        // Each holder's part: the redemption each of its burns paid for, the
        // first after the burn in the same transaction for the same shares.
        let mut counted = HashSet::new();
        for log in logs {
            let Ok(burnt) = log.log_decode::<IVault::Transfer>() else {
                continue;
            };
            let burnt = burnt.inner.data;
            if burnt.to != Address::ZERO || burnt.from == Address::ZERO {
                continue;
            }
            let paid_for = redeemed_closed
                .iter()
                .filter(|(redeemed, after)| {
                    redeemed.shares == burnt.value
                        && after
                            .zip(log.log_index)
                            .is_some_and(|(after, index)| after > index)
                        && !counted.contains(after)
                })
                .min_by_key(|(_, after)| *after);
            if let Some((redeemed, place)) = paid_for {
                counted.insert(*place);
                *self.paid.entry(burnt.from).or_default() -= dollars(redeemed.assets);
            }
        }
    }
}

// The record -----------------------------------------------------------------

/// Readings at past blocks this refresh asked for and could not take, by
/// what they were for: a paid claim's supply, by vault and id.
#[derive(Default)]
pub struct PastReads {
    /// The supply each claim was paid from that could not be read this
    /// refresh, with why.
    pub failed: HashMap<(Address, u64), String>,
}

/// Every vault, as the site shows it. `vault_chain` is the chain these
/// vaults live on, by the name a manifest gives it. `now` is the chain time
/// of the observation's block. Fails when the factory's list of vaults can't
/// be read.
#[allow(clippy::too_many_arguments)]
pub fn strategies(
    views: &Views,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    now: u64,
    read_through: bool,
    past: &PastReads,
) -> Result<Vec<Strategy>> {
    let addresses = views.read(deployments.factory, IVaultFactory::vaultsCall {})?;

    let mut out = Vec::new();
    for address in &addresses {
        // Where each vault was created, so its record starts there: until the
        // factory's log has been read through, a vault not found in it yet may
        // still be.
        let created_in = match history.activated.get(address) {
            Some(block) => Some((*block).max(deployments.deployed_block)),
            // Read through and not there: no vault existed before the
            // platform did.
            None if read_through => Some(deployments.deployed_block),
            None => None,
        };
        match one(
            views,
            vault_chain,
            deployments,
            store,
            history,
            health,
            *address,
            created_in,
            now,
            past,
        ) {
            Ok(strategy) => {
                history.last_whole.insert(*address, (strategy.clone(), now));
                out.push(strategy);
            }
            // A vault that can't be read whole stays shown as it was last
            // read, marked with when that was, rather than dropped; one never
            // read whole is shown as not read yet (see [`unread`]).
            Err(error) => match history.last_whole.get(address) {
                Some((strategy, read_at)) => {
                    tracing::warn!(vault = %address, %error, "could not read a vault; kept it as last read");
                    out.push(Strategy {
                        stale_as_of: Some(moment(*read_at)),
                        ..strategy.clone()
                    });
                }
                None => {
                    tracing::warn!(vault = %address, %error, "could not read a vault never read whole; shown as not read yet");
                    out.push(unread(views, vault_chain, deployments, *address));
                }
            },
        }
    }

    Ok(out)
}

/// The vaults the factory lists, as the observation read them.
pub fn vaults(views: &Views, deployments: &Deployments) -> Result<Vec<Address>> {
    views.read(deployments.factory, IVaultFactory::vaultsCall {})
}

/// When a vault never read whole was last read whole: the Unix epoch, which
/// no chain read is dated at. An entry so marked carries no figures (see
/// [`unread`]).
pub const NEVER_READ: PrimitiveDateTime = PrimitiveDateTime::new(
    OffsetDateTime::UNIX_EPOCH.date(),
    OffsetDateTime::UNIX_EPOCH.time(),
);

/// Whether an entry is a vault the factory lists that has never been read
/// whole: its identity, as far as it could be read, and no figures.
pub fn never_read(strategy: &Strategy) -> bool {
    strategy.stale_as_of == Some(NEVER_READ)
}

/// A vault the factory lists that has never been read whole, as the record
/// shows it: by its address and what could be read of who it is (its
/// strategy id and author), marked stale as of [`NEVER_READ`], with no
/// figures. It counts toward its chain's staleness as any stale vault does,
/// and toward no total.
fn unread(
    views: &Views,
    vault_chain: Chain,
    deployments: &Deployments,
    address: Address,
) -> Strategy {
    let id: u64 = match views.read(address, IVault::strategyIdCall {}) {
        Ok(id) => id.try_into().unwrap_or_default(),
        Err(_) => 0,
    };
    let (author, author_address) = match views.read(address, IVault::authorCall {}) {
        Ok(author) => (handle_of(views, deployments, author), address_of(author)),
        Err(_) => (String::new(), String::new()),
    };
    placeholder(vault_chain, id, author, author_address, address_of(address))
}

/// A vault never read whole, as the record shows it: who it is as far as
/// that could be read, and no figures (see [`unread`]).
pub(crate) fn placeholder(
    vault_chain: Chain,
    id: u64,
    author: String,
    author_address: String,
    address: String,
) -> Strategy {
    let unknown = || "not read yet".to_string();
    let epoch = NEVER_READ.date();
    Strategy {
        id,
        slug: Strategy::slug_for(vault_chain, id),
        title: if id == 0 {
            format!("A vault at {address}")
        } else {
            format!("Agent {id}")
        },
        description: "Its vault has not been read whole yet.".to_string(),
        claims: Vec::new(),
        author,
        author_address,
        standing: Standing::New,
        registered: epoch,
        days_live: 0,
        bundle: Bundle {
            bundle_hash: unknown(),
            component_hash: unknown(),
            source_hash: unknown(),
            manifest_hash: unknown(),
            world: unknown(),
            toolchain: unknown(),
            builder_image: unknown(),
            models: Vec::new(),
            source: shared::model::bundle::Source::Sealed,
            source_files: Vec::new(),
            built_in_seconds: 0,
        },
        vault: Vault {
            address,
            chain: vault_chain,
            asset: vault_chain.dollar().to_string(),
            state: VaultState::Activating,
            created: epoch,
            station: String::new(),
            value: 0.0,
            valued_at: None,
            idle: 0.0,
            escrow: 0.0,
            shares: 0.0,
            share_price: 0.0,
            high_water_mark: 0.0,
            last_crystallised: epoch,
            idle_floor_bps: 0,
            pending_floor: None,
            allowances: shared::Allowances {
                fuel_per_sec: 0,
                memory_mib: 0,
                requests_per_min: 0,
                bytes_per_day: 0,
                streams: 0,
                inference_ms_per_min: 0,
                gas_per_day: 0.0,
            },
            requests: Vec::new(),
            restricted: Vec::new(),
            halt: None,
            fees: Fees {
                holding_bps: 0,
                author_bps: 0,
                platform_bps: 0,
                transaction_bps: 0,
            },
            exit_band_bps: 0,
            tank: Tank {
                balance: 0.0,
                daily_charge: 0.0,
                keep_at: 0.0,
                usage_settled: 0.0,
            },
            exit_class: ExitClass::Direct,
            freeze_after_days: 0,
            claims: Vec::new(),
            in_transit: None,
        },
        legs: Vec::new(),
        review: None,
        health: None,
        attestation: None,
        record: None,
        trades: Vec::new(),
        events: Vec::new(),
        kill: None,
        restricted: Vec::new(),
        drivers: Vec::new(),
        bridges: Vec::new(),
        stale_as_of: Some(NEVER_READ),
    }
}

#[allow(clippy::too_many_arguments)]
fn one(
    views: &Views,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    address: Address,
    created_in: Option<u64>,
    now: u64,
    past: &PastReads,
) -> Result<Strategy> {
    let strategy_id = views.read(address, IVault::strategyIdCall {})?;
    let id: u64 = strategy_id.try_into().unwrap_or_default();
    let author = views.read(address, IVault::authorCall {})?;
    let created = views.read(address, IVault::createdAtCall {})?;
    let state = views.read(address, IVault::stateCall {})?;
    let bundle_hash = views.read(address, IVault::bundleHashCall {})?;
    let registered = views.read(
        deployments.strategies,
        IStrategyRegistry::strategyOfCall {
            strategyId: strategy_id,
        },
    )?;

    let slug = Strategy::slug_for(vault_chain, id);
    let days_live = (now.saturating_sub(created)) / 86_400;

    let events = read_events(history, address, created_in);
    let vault = read_vault(views, vault_chain, history, address, &state, past)?;
    let station: Address = vault.station.parse().unwrap_or_default();
    let (trades, trade_count) = read_trades(deployments, history, station, created_in);
    let record = build_record(history, address, &vault, now, trade_count);
    let legs = read_legs(views, deployments, &vault)?;
    let review = read_review(views, deployments, store, strategy_id)?;
    let attestation = read_attestation(
        views,
        deployments,
        history,
        address,
        created_in.is_some(),
        now,
    )?;
    let bundle = read_bundle(store, &registered, bundle_hash);

    let kill = history
        .vaults
        .get(&address)
        .and_then(|logs| logs.killed.as_ref())
        .map(|event| Kill {
            at: event.at,
            reason: "the admin key stopped it".to_string(),
            outcome: if matches!(state, ChainVaultState::Closed) {
                "every position was closed and the cash brought home; holders are paid from it"
                    .to_string()
            } else {
                "its positions are being closed and the cash brought home".to_string()
            },
            reference: event.reference.clone(),
        });
    // A vault the admin stopped stays shown as stopped once it has closed:
    // that is what happened to it.
    let standing = match standing(&state, days_live) {
        Standing::Closed if kill.is_some() => Standing::Killed,
        other => other,
    };

    let manifest = store
        .manifest(&hash_of(registered.manifestHash))
        .map(|canonical| canonical.manifest);
    Ok(Strategy {
        id,
        slug,
        title: manifest
            .as_ref()
            .map(|manifest| manifest.description.title.clone())
            .unwrap_or_else(|| format!("Agent {id}")),
        description: manifest
            .as_ref()
            .map(|manifest| manifest.description.text.clone())
            .unwrap_or_else(|| "Its manifest is not in the store.".to_string()),
        claims: manifest
            .as_ref()
            .map(|manifest| manifest.description.claims.clone())
            .unwrap_or_default(),
        author: handle_of(views, deployments, author),
        author_address: address_of(author),
        standing,
        registered: day(registered.registeredAt),
        days_live: days_live as u32,
        bundle,
        vault,
        legs,
        review,
        health: health.get(&address).cloned(),
        attestation,
        record,
        trades,
        events,
        kill,
        restricted: registered
            .restricted
            .iter()
            .map(|code| name_of(*code))
            .collect(),
        stale_as_of: None,
        drivers: registered
            .chains
            .iter()
            .flat_map(|need| need.drivers.iter().map(|driver| name_of(*driver)))
            .collect(),
        bridges: registered
            .bridges
            .iter()
            .map(|bridge| name_of(*bridge))
            .collect(),
    })
}

fn standing(state: &ChainVaultState, days_live: u64) -> Standing {
    match state {
        ChainVaultState::Killed => Standing::Killed,
        ChainVaultState::Frozen => Standing::Frozen,
        ChainVaultState::Closed | ChainVaultState::Cancelled => Standing::Closed,
        ChainVaultState::WindingDown => Standing::Retiring,
        _ if days_live >= Strategy::DAYS_NEW as u64 => Standing::Listed,
        _ => Standing::New,
    }
}

fn read_vault(
    views: &Views,
    vault_chain: Chain,
    history: &mut History,
    address: Address,
    state: &ChainVaultState,
    past: &PastReads,
) -> Result<Vault> {
    let fees = views.read(address, IVault::feesCall {})?;
    let tank = views.read(address, IVault::tankCall {})?;
    let cash = views.read(address, IVault::cashCall {})?;
    let escrow = views.read(address, IVault::escrowCall {})?;
    let share_price = views.read(address, IVault::sharePriceCall {})?;
    let last_priced = views.read(address, IVault::lastPricedCall {})?;
    let mark = views.read(address, IVault::markCall {})?;
    let last_payment = views.read(address, IVault::lastPaymentCall {})?;
    let created = views.read(address, IVault::createdAtCall {})?;
    let timers = views.read(address, IVault::timersCall {})?;
    let exit_band = views.read(address, IVault::exitBandBpsCall {})?;
    let station = views.read(address, IVault::homeStationCall {})?;
    let floor = views.read(address, IVault::idleFloorCall {})?;
    let allowances = views.read(address, IVault::allowancesCall {})?;
    let restricted = views.read(address, IVault::restrictedCall {})?;
    let halted = views.read(address, IStationVault::haltedAtCall {})?;
    let supply = views.read(address, IVault::totalSupplyCall {})?;

    let mut claims = Vec::new();
    let count: u64 = views
        .read(address, IVault::claimCountCall {})?
        .try_into()
        .unwrap_or(0);
    for id in 1..=count {
        if let Some(claim) = history
            .paid_claims
            .get(&address)
            .and_then(|paid| paid.get(&id))
        {
            claims.push(claim.clone());
            continue;
        }
        let claim = views.read(
            address,
            IVault::claimAtCall {
                claimId: U256::from(id),
            },
        )?;
        let (fraction, measured) = if matches!(claim.state, ChainClaimState::Paid) {
            closed_by_paid(views, history, address, id, claim.shares, supply, past)
        } else {
            let fraction = fraction_of(claim.shares, supply);
            history
                .waiting
                .entry(address)
                .or_default()
                .insert(id, fraction);
            (fraction, false)
        };
        let claim = Claim {
            id: id as u32,
            owner: address_of(claim.owner),
            shares: shares(claim.shares),
            fraction_closed: fraction,
            booked: day(claim.bookedAt),
            state: match claim.state {
                ChainClaimState::Paid => ClaimState::Paid,
                ChainClaimState::Waiting => ClaimState::Waiting,
                ChainClaimState::Queued => ClaimState::Queued,
                ChainClaimState::Fallback => ClaimState::Fallback,
                _ => ClaimState::Pending,
            },
            settled_for: (claim.paid > U256::ZERO).then(|| dollars(claim.paid)),
        };
        // Kept for good only once measured against the supply its payment
        // was made from, or once that supply has gone unanswered too long;
        // until then it is read again at each refresh.
        if claim.state == ClaimState::Paid && measured {
            history
                .paid_claims
                .entry(address)
                .or_default()
                .insert(id, claim.clone());
            if let Some(waiting) = history.waiting.get_mut(&address) {
                waiting.remove(&id);
            }
        }
        claims.push(claim);
    }

    // A depositor's waiting request unread is not a request gone: the read
    // fails, and the vault is shown as it was last read whole, marked stale.
    let requests = read_requests(views, history, address)?;

    let state = match state {
        ChainVaultState::Activating => VaultState::Activating,
        ChainVaultState::Open => VaultState::Open,
        ChainVaultState::WindingDown => VaultState::WindingDown,
        ChainVaultState::Frozen => VaultState::Frozen,
        ChainVaultState::Killed => VaultState::Killed,
        ChainVaultState::Cancelled => VaultState::Cancelled,
        _ => VaultState::Closed,
    };
    // Every share at the last valuation's price, the price each request is
    // priced at: a deposit priced since then is in it as the shares it
    // made, and dollars still in escrow are not. A closed vault is worth
    // the cash its holders are paid from.
    let value = if state == VaultState::Closed {
        dollars(cash)
    } else {
        shares(supply) * dollars(share_price)
    };
    let daily = shared::Prices::published().daily_cap(&shared_allowances(&allowances));
    Ok(Vault {
        address: address_of(address),
        chain: vault_chain,
        asset: vault_chain.dollar().to_string(),
        state,
        created: day(created),
        station: address_of(station),
        value,
        valued_at: last_priced.priced.then(|| moment(last_priced.time)),
        idle: dollars(cash),
        escrow: dollars(escrow),
        shares: shares(supply),
        share_price: dollars(share_price),
        // `1e18` is $1.00 a share.
        high_water_mark: shares(mark),
        last_crystallised: day(last_payment),
        idle_floor_bps: floor.bps as u32,
        pending_floor: (floor.pendingAt != 0).then(|| PendingFloor {
            bps: floor.pendingBps as u32,
            from: moment(floor.pendingAt),
        }),
        allowances: shared_allowances(&allowances),
        requests,
        restricted: restricted.iter().map(|code| name_of(*code)).collect(),
        halt: if halted != 0 {
            Some(Halt {
                at: moment(halted),
                reason: halt_reason(history, address),
            })
        } else {
            None
        },
        fees: Fees {
            holding_bps: fees.holdingBps as u32,
            author_bps: fees.authorBps as u32,
            platform_bps: fees.platformBps as u32,
            transaction_bps: fees.transactionBps as u32,
        },
        exit_band_bps: exit_band as u32,
        tank: Tank {
            balance: dollars(tank.balance),
            daily_charge: daily,
            keep_at: dollars(tank.keepAt),
            usage_settled: dollars(tank.usageSettled),
        },
        // Every venue this vault trades is on its own chain, so the station
        // serves every claim itself and nobody has to be there for it.
        exit_class: ExitClass::Direct,
        freeze_after_days: (timers.freezeAfter / 86_400) as u32,
        claims,
        in_transit: None,
    })
}

fn fraction_of(claimed: U256, supply: U256) -> f64 {
    if supply.is_zero() {
        0.0
    } else {
        shares(claimed) / shares(supply)
    }
}

/// The share of the vault a paid claim closed, and whether that is what it
/// closed for good. Paying it burned its shares, so the supply now no longer
/// counts them: it is measured against the supply the block before it was
/// paid, less the shares of the claims paid before it in the same block, once
/// its payment is in the vault's log and that supply has been read (at the
/// block before, apart: see [`History::unmeasured_payments`]). Until then
/// (the log not read that far yet, or the supply then not answered) it is
/// the share it stood for when last read waiting, or today's supply with its
/// own shares put back, and is worked out again at the next refresh; once
/// the supply then has gone unanswered [`UNANSWERED_TRIES`] refreshes in a
/// row (a node that keeps no state that far back), that share is kept for
/// good, so a claim no node will measure isn't read again at every refresh
/// for ever.
fn closed_by_paid(
    views: &Views,
    history: &mut History,
    vault: Address,
    id: u64,
    claimed: U256,
    supply: U256,
    past: &PastReads,
) -> (f64, bool) {
    let paid_in = history.vaults.get(&vault).map(|logs| &logs.paid_in);
    let mut given_up = false;
    if let Some(paid) = paid_in.and_then(|paid_in| paid_in.get(&id)) {
        let measured = match history.supplies.get(&(vault, id)) {
            Some(before) => {
                supply_before(views, vault, *before, *paid, paid_in.into_iter().flatten())
            }
            None => Err(anyhow::anyhow!(
                "{}",
                past.failed
                    .get(&(vault, id))
                    .cloned()
                    .unwrap_or_else(|| "the supply before it is not read yet".to_string())
            )),
        };
        match measured {
            Ok(before) => {
                history.unmeasured.remove(&(vault, id));
                return (fraction_of(claimed, before), true);
            }
            Err(error) => {
                let tries = history.unmeasured.entry((vault, id)).or_insert(0);
                *tries += 1;
                if *tries >= UNANSWERED_TRIES {
                    tracing::warn!(%vault, claim = id, %error, "the supply a paid claim was paid from was never answered; kept what it was last read to close");
                    history.unmeasured.remove(&(vault, id));
                    given_up = true;
                }
            }
        }
    }
    let fraction = match history
        .waiting
        .get(&vault)
        .and_then(|waiting| waiting.get(&id))
    {
        Some(fraction) => *fraction,
        None => fraction_of(claimed, supply.saturating_add(claimed)),
    };
    (fraction, given_up)
}

/// The vault's supply just before the payment `paid`: as it was the block
/// before (`at_block_before`), less the shares of every claim paid ahead of
/// it in its block.
fn supply_before<'a>(
    views: &Views,
    vault: Address,
    at_block_before: U256,
    paid: Paid,
    every: impl Iterator<Item = (&'a u64, &'a Paid)>,
) -> Result<U256> {
    let mut supply = at_block_before;
    for (earlier, _) in
        every.filter(|(_, other)| other.block == paid.block && other.log_index < paid.log_index)
    {
        let claim = views.read(
            vault,
            IVault::claimAtCall {
                claimId: U256::from(*earlier),
            },
        )?;
        supply = supply.saturating_sub(claim.shares);
    }
    Ok(supply)
}

/// What is known of a vault's requests from one refresh to the next: a
/// request once done stays done, so it is not read again.
#[derive(Clone, Default)]
struct Requests {
    /// The first request not known to be done: every one before it is.
    from: u64,
    /// Requests past `from` known to be done.
    done: std::collections::HashSet<u64>,
}

/// The most of a vault's requests read in one refresh. A vault with more
/// still to read than this is read a page at a time, the rest at the next
/// refresh; until every one has been read it is not read whole.
const REQUEST_READS: u32 = 256;

/// Every request still waiting to be priced, oldest first: each request from
/// the first not known to be done to the latest, those known done passed
/// over. A refresh reads at most [`REQUEST_READS`] of them; one with more to
/// read fails, so the vault is shown as last read whole, marked stale, never
/// with part of its waiting requests as if they were all.
fn read_requests(views: &Views, history: &mut History, address: Address) -> Result<Vec<Request>> {
    let count: u64 = views
        .read(address, IVault::requestCountCall {})?
        .try_into()
        .unwrap_or(0);
    let known = history.requests.entry(address).or_default();
    known.from = known.from.max(1);
    let read = waiting_requests(views, known, address, count);
    // Every request before the first not done is done.
    while known.done.remove(&known.from) {
        known.from += 1;
    }
    read
}

fn waiting_requests(
    views: &Views,
    known: &mut Requests,
    address: Address,
    count: u64,
) -> Result<Vec<Request>> {
    let mut waiting = Vec::new();
    let mut reads = 0;
    for id in known.from..=count {
        if known.done.contains(&id) {
            continue;
        }
        if reads == REQUEST_READS {
            anyhow::bail!(
                "the vault's requests from {id} to {count} are still to be read; the rest at the next refresh"
            );
        }
        reads += 1;
        let request = views.read(
            address,
            IVault::requestAtCall {
                requestId: U256::from(id),
            },
        )?;
        if request.done {
            known.done.insert(id);
            continue;
        }
        let kind = match request.kind {
            ChainRequestKind::Deposit => RequestKind::Deposit,
            ChainRequestKind::Redeem => RequestKind::Withdrawal,
            ChainRequestKind::Claim => RequestKind::Claim,
            _ => RequestKind::TankFill,
        };
        let in_dollars = matches!(kind, RequestKind::Deposit)
            || (matches!(kind, RequestKind::TankFill) && request.r#ref == U256::from(1u64));
        waiting.push(Request {
            id,
            owner: address_of(request.owner),
            kind,
            amount: if in_dollars {
                dollars(request.amount)
            } else {
                shares(request.amount)
            },
            earliest: moment(request.earliest),
        });
    }
    Ok(waiting)
}

fn shared_allowances(on_chain: &chain::bindings::prices::Allowances) -> shared::Allowances {
    shared::Allowances {
        fuel_per_sec: on_chain.fuelPerSec,
        memory_mib: on_chain.memoryMib,
        requests_per_min: on_chain.requestsPerMin,
        bytes_per_day: on_chain.bytesPerDay,
        streams: on_chain.streams,
        inference_ms_per_min: on_chain.inferenceMsPerMin,
        gas_per_day: on_chain.gasPerDay as f64 / 1_000_000.0,
    }
}

// Readings at past blocks ----------------------------------------------------

/// Reads at past blocks, for a vault's hourly readings.
pub trait Past {
    /// Block `number`'s header.
    fn header(
        &mut self,
        number: u64,
    ) -> impl std::future::Future<Output = Result<chain::follow::Header>> + Send;
    /// The vault's value per share and what it was worth, at `block`.
    fn prices(
        &mut self,
        vault: Address,
        block: chain::follow::Header,
    ) -> impl std::future::Future<Output = Result<(f64, f64)>> + Send;
}

/// Take the vault's readings due since the last: one an hour of chain time
/// (one a day for what is older than [`HOURLY_FOR`]), from the block it was
/// created in on. Each reading is taken at the first block whose time is
/// due, searched for between the last reading's block and the head: guessed
/// from the pace the chain kept between the two ends, and halved instead
/// whenever a guess fails to halve what is left, so a chain whose pace
/// changes is searched in as many reads as halving would take. A refresh
/// asks for at most [`READS_PER_REFRESH`] block times, and the next carries
/// on from where it stopped. A reading whose block's time can't be read, or
/// that the vault doesn't answer, is not taken: it is asked for again at the
/// next refresh, never dated by a guess. `created_in` is `None` while the
/// block the vault was created in isn't known yet. `head` is the block the
/// record is read at.
pub async fn sample<P: Past>(
    past: &mut P,
    address: Address,
    created_in: Option<u64>,
    history: &mut History,
    head: chain::follow::Header,
) {
    let now = head.timestamp;
    let mut headers: HashMap<u64, chain::follow::Header> = HashMap::from([(head.number, head)]);
    let mut reads = 0;
    let mut last = match history.sampled.get(&address) {
        Some(last) => *last,
        None => {
            let Some(start) = created_in else {
                return;
            };
            let block = start.clamp(1, head.number.max(1));
            reads += 1;
            let Ok(header) = header_at(past, &mut headers, block).await else {
                return;
            };
            let at = Mark {
                block,
                time: header.timestamp,
            };
            match take(past, history, address, at, header).await {
                Some(taken) => taken,
                None => return,
            }
        }
    };

    'readings: while reads < READS_PER_REFRESH {
        let spacing = if now.saturating_sub(last.time) > HOURLY_FOR {
            DAY
        } else {
            HOUR
        };
        let due = last.time.saturating_add(spacing);
        if due > now || last.block >= head.number {
            break;
        }
        // The first block at or after `due` lies past `low`, whose time is
        // before it, and no further than `high`, whose time is not.
        let mut low = last;
        let mut high = Mark {
            block: head.number,
            time: now,
        };
        let mut halve = false;
        while high.block - low.block > 1 {
            if reads >= READS_PER_REFRESH {
                break 'readings;
            }
            let left = high.block - low.block;
            let ahead = if halve {
                left / 2
            } else {
                blocks_until(low, high, due).min(left - 1)
            };
            reads += 1;
            let Ok(header) = header_at(past, &mut headers, low.block + ahead).await else {
                break 'readings;
            };
            let mark = Mark {
                block: low.block + ahead,
                time: header.timestamp,
            };
            if mark.time >= due {
                high = mark;
            } else {
                low = mark;
            }
            halve = (high.block - low.block) * 2 > left;
        }
        let Some(header) = headers.get(&high.block).copied() else {
            break;
        };
        match take(past, history, address, high, header).await {
            Some(taken) => last = taken,
            None => break,
        }
    }
    prune(history.samples.entry(address).or_default(), now);
}

async fn header_at<P: Past>(
    past: &mut P,
    headers: &mut HashMap<u64, chain::follow::Header>,
    number: u64,
) -> Result<chain::follow::Header> {
    if let Some(header) = headers.get(&number) {
        return Ok(*header);
    }
    let header = past.header(number).await?;
    headers.insert(number, header);
    Ok(header)
}

/// How many blocks past `low` the time `due` falls, at the pace the chain
/// kept between `low` and `high`: at least one, and no further than `high`.
fn blocks_until(low: Mark, high: Mark, due: u64) -> u64 {
    let blocks = (high.block - low.block) as u128;
    let seconds = high.time.saturating_sub(low.time).max(1) as u128;
    let wanted = due.saturating_sub(low.time) as u128;
    let guess = (wanted * blocks).div_ceil(seconds);
    (guess.clamp(1, blocks)) as u64
}

/// Read the vault at `at` and keep the reading. One the vault doesn't
/// answer is not taken, and is asked for again at the next refresh; one it
/// has left unanswered [`UNANSWERED_TRIES`] refreshes in a row is passed
/// over, so a block it can't be read at holds up no reading after it. The
/// block is where the next reading is looked for from, unless this one is
/// to be asked for again.
async fn take<P: Past>(
    past: &mut P,
    history: &mut History,
    address: Address,
    at: Mark,
    header: chain::follow::Header,
) -> Option<Mark> {
    match past.prices(address, header).await {
        Ok((value_per_share, aum)) => {
            history.samples.entry(address).or_default().push(Sample {
                at: at.time,
                value_per_share,
                aum,
            });
        }
        Err(error) => {
            let tries = history.unanswered.entry(address).or_insert((at.block, 0));
            if tries.0 != at.block {
                *tries = (at.block, 0);
            }
            tries.1 += 1;
            if tries.1 < UNANSWERED_TRIES {
                return None;
            }
            tracing::warn!(vault = %address, block = at.block, error = format!("{error:#}"), "a reading the vault never answered was passed over");
        }
    }
    history.unanswered.remove(&address);
    history.sampled.insert(address, at);
    Some(at)
}

/// Keep readings older than [`HOURLY_FOR`] one a day, the last of each day.
pub(crate) fn prune(samples: &mut Vec<Sample>, now: u64) {
    let hourly_from = now.saturating_sub(HOURLY_FOR);
    let mut kept: Vec<Sample> = Vec::with_capacity(samples.len());
    for sample in samples.drain(..) {
        match kept.last_mut() {
            Some(last) if sample.at < hourly_from && day(last.at) == day(sample.at) => {
                *last = sample
            }
            _ => kept.push(sample),
        }
    }
    *samples = kept;
}

/// The record: the vault's readings, one a day, with today's from the vault
/// as just read.
fn build_record(
    history: &History,
    address: Address,
    vault: &Vault,
    now: u64,
    trades: u32,
) -> Option<Record> {
    let samples = history
        .samples
        .get(&address)
        .map(Vec::as_slice)
        .unwrap_or_default();
    record_from(samples, vault, now, trades)
}

/// The record from a vault's readings, oldest first, with today's from the
/// vault as just read.
pub(crate) fn record_from(
    samples: &[Sample],
    vault: &Vault,
    now: u64,
    trades: u32,
) -> Option<Record> {
    let mut value_per_share: Vec<Point> = samples
        .iter()
        .map(|sample| Point {
            date: day(sample.at),
            value: sample.value_per_share,
        })
        .collect();
    let mut aum: Vec<Point> = samples
        .iter()
        .map(|sample| Point {
            date: day(sample.at),
            value: sample.aum,
        })
        .collect();
    // A vault with no readings yet still has the day it was created.
    if value_per_share.is_empty() {
        value_per_share.push(Point {
            date: vault.created,
            value: 1.0,
        });
    }
    let today = day(now);
    if value_per_share.last().is_none_or(|last| last.date <= today) {
        value_per_share.push(Point {
            date: today,
            value: vault.share_price,
        });
        aum.push(Point {
            date: today,
            value: vault.value,
        });
    }
    let value_per_share = daily(&value_per_share);
    if value_per_share.len() < 2 {
        return None;
    }
    Some(Record::from_series(value_per_share, daily(&aum), trades))
}

/// One reading a day: the record is daily, and a day with several readings
/// keeps its last.
fn daily(points: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    for point in points {
        match out.last_mut() {
            Some(last) if last.date == point.date => *last = *point,
            _ => out.push(*point),
        }
    }
    out
}

/// What a vault's price and worth are read as at a past block: its share
/// price and its total value then.
pub fn price_reads(vault: Address) -> chain::follow::Plan {
    let mut plan = chain::follow::Plan::default();
    plan.read(vault, &IVault::sharePriceCall {});
    plan.read(vault, &IVault::totalValueCall {});
    plan
}

/// The answers to [`price_reads`].
pub fn prices_from(observed: &chain::follow::Observed) -> Result<(f64, f64)> {
    let price = observed.decode::<IVault::sharePriceCall>(0)?;
    let value = observed.decode::<IVault::totalValueCall>(1)?;
    Ok((dollars(price), dollars(value)))
}

/// Every fill the vault's home station has had (the vault never trades
/// itself: its station is the venue account the agent trades), as the
/// stand-in venue's logs have been read so far: the last few in full, for
/// the page, and how many there were altogether, for the record. The station
/// was made with its vault, so none are shown while the block the vault was
/// created in isn't known yet.
fn read_trades(
    deployments: &Deployments,
    history: &History,
    station: Address,
    created_in: Option<u64>,
) -> (Vec<Trade>, u32) {
    // The stand-in venue's fills alone are read here; a public test
    // network's deployment has no stand-in venue.
    if station == Address::ZERO || deployments.venue == Address::ZERO || created_in.is_none() {
        return (Vec::new(), 0);
    }
    match history.fills.get(&station) {
        Some(fills) => (fills.newest.iter().rev().cloned().collect(), fills.count),
        None => (Vec::new(), 0),
    }
}

/// How many fills a vault's home station has had altogether, as last read:
/// every one, where the page shows only the newest.
pub fn trade_count(history: &History, strategy: &Strategy) -> u32 {
    strategy
        .vault
        .station
        .parse::<Address>()
        .ok()
        .and_then(|station| history.fills.get(&station))
        .map_or(strategy.trades.len() as u32, |fills| fills.count)
}

/// Why the vault halted, from its last `Halted` log, in words.
fn halt_reason(history: &History, address: Address) -> String {
    let Some(reason) = history.vaults.get(&address).and_then(|logs| logs.halted) else {
        return String::new();
    };
    match name_of(reason).as_str() {
        "killed" => "the admin key stopped it".to_string(),
        "frozen" => "a claim went unpaid for its whole freeze period".to_string(),
        "retired" => "its author retired it and the wind-down ended".to_string(),
        "sealed" => "it is being wound down".to_string(),
        other => other.to_string(),
    }
}

/// The vault's events the page shows, newest first, as its logs have been
/// read so far; none while the block it was created in isn't known yet.
fn read_events(history: &History, address: Address, created_in: Option<u64>) -> Vec<VaultEvent> {
    if created_in.is_none() {
        return Vec::new();
    }
    history
        .vaults
        .get(&address)
        .map(|logs| logs.events.iter().rev().cloned().collect())
        .unwrap_or_default()
}

/// The admin's kill of a vault that had already halted (frozen, or its
/// wind-down ended): the halt is kept as it was, so the kill logs no
/// `Halted`, only its `StateChanged(Killed)`. A kill of a vault still
/// running halts it in the same transaction, and that `Halted` tells of it.
fn killed_after_halt(made: &VaultLogs, log: &Log) -> Option<VaultEvent> {
    let changed = log.log_decode::<IVault::StateChanged>().ok()?;
    if !matches!(changed.inner.data.state, ChainVaultState::Killed)
        || made.halted_in == Some((log.block_number, log.transaction_hash))
    {
        return None;
    }
    Some(VaultEvent {
        at: moment(log.block_timestamp.unwrap_or_default()),
        kind: EventKind::Killed,
        summary: "the admin key stopped the vault after it had halted".to_string(),
        amount: None,
        reference: log
            .transaction_hash
            .map(|hash| format!("{hash:?}"))
            .unwrap_or_default(),
    })
}

/// A vault's log as the page tells of it, if it tells of it at all.
fn vault_event(log: &Log) -> Option<VaultEvent> {
    let at = moment(log.block_timestamp.unwrap_or_default());
    let reference = log
        .transaction_hash
        .map(|hash| format!("{hash:?}"))
        .unwrap_or_default();
    let (kind, summary, amount) = if let Ok(event) = log.log_decode::<IVault::Requested>() {
        let event = event.inner.data;
        match event.kind {
            ChainRequestKind::Deposit => (
                EventKind::Requested,
                "a deposit was asked for, to be priced at a later valuation".to_string(),
                Some(dollars(event.amount)),
            ),
            ChainRequestKind::Redeem => (
                EventKind::Requested,
                "a withdrawal was asked for, to be priced at a later valuation".to_string(),
                None,
            ),
            ChainRequestKind::Claim => (
                EventKind::Requested,
                "a claim the cash covers was queued, to be priced at the next valuation"
                    .to_string(),
                None,
            ),
            _ => (
                EventKind::Requested,
                "the tank asked to be topped up from the author's shares".to_string(),
                None,
            ),
        }
    } else if let Ok(event) = log.log_decode::<IVault::ClaimTried>() {
        let event = event.inner.data;
        (
            EventKind::ClaimTried,
            if event.blocked {
                format!(
                    "claim {} was tried; a market outside the exit band held its close",
                    event.claimId
                )
            } else {
                format!("claim {} was tried", event.claimId)
            },
            None,
        )
    } else if let Ok(event) = log.log_decode::<IVault::ProRataTaken>() {
        (
            EventKind::FallbackTaken,
            "a holder nothing could price took the pro-rata fallback".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::UnitsPaid>() {
        (
            EventKind::FallbackTaken,
            "a fallback holder was paid for their units".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::StateChanged>() {
        match event.inner.data.state {
            ChainVaultState::Closed => (
                EventKind::Closed,
                "the vault closed; its holders are paid from its cash".to_string(),
                None,
            ),
            ChainVaultState::Cancelled => (
                EventKind::Closed,
                "the activation was cancelled and the tank returned".to_string(),
                None,
            ),
            _ => return None,
        }
    } else if let Ok(event) = log.log_decode::<IVault::Deposited>() {
        (
            EventKind::Deposited,
            "a deposit was priced".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::Withdrawn>() {
        (
            EventKind::Withdrawn,
            "a withdrawal was priced and paid".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::Refunded>() {
        (
            EventKind::Refunded,
            "a deposit the vault could no longer take went back".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::ClaimBooked>() {
        (
            EventKind::ClaimBooked,
            format!(
                "a withdrawal the cash could not cover: claim {}",
                event.inner.data.claimId
            ),
            None,
        )
    } else if let Ok(event) = log.log_decode::<IVault::ClaimPaid>() {
        (
            EventKind::ClaimSettled,
            format!("claim {} paid", event.inner.data.claimId),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::ClaimPartPaid>() {
        (
            EventKind::ClaimSettled,
            format!("claim {} paid in part", event.inner.data.claimId),
            Some(dollars(event.inner.data.assets)),
        )
    } else if log.log_decode::<IVault::FeesPaid>().is_ok() {
        (
            EventKind::Crystallised,
            "the week's fees were paid as new shares".to_string(),
            None,
        )
    } else if let Ok(event) = log.log_decode::<IVault::ToStation>() {
        (
            EventKind::CapitalMoved,
            "cash to the station".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::FromStation>() {
        (
            EventKind::CapitalMoved,
            "the station to cash".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::TankFunded>() {
        (
            EventKind::TankFunded,
            "the tank was topped up".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::TankFilled>() {
        (
            EventKind::TankFunded,
            "the tank was topped up from the author's shares".to_string(),
            Some(dollars(event.inner.data.assets)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::TankCharged>() {
        (
            EventKind::TankCharged,
            "the tank paid for running the agent".to_string(),
            Some(dollars(event.inner.data.charged)),
        )
    } else if let Ok(event) = log.log_decode::<IVault::Halted>() {
        match name_of(event.inner.data.reason).as_str() {
            "frozen" => (
                EventKind::Frozen,
                "a claim went untried and the vault froze".to_string(),
                None,
            ),
            "retired" => (
                EventKind::Retired,
                "the author's wind-down ended".to_string(),
                None,
            ),
            "sealed" => (
                EventKind::Killed,
                "the vault is being wound down".to_string(),
                None,
            ),
            _ => (EventKind::Killed, "the vault was stopped".to_string(), None),
        }
    } else {
        return None;
    };
    Some(VaultEvent {
        at,
        kind,
        summary,
        amount,
        reference,
    })
}

fn read_legs(views: &Views, deployments: &Deployments, vault: &Vault) -> Result<Vec<Leg>> {
    // The stand-in venue's leg alone is read here; a public test network's
    // deployment has none.
    if deployments.venue == Address::ZERO {
        return Ok(Vec::new());
    }
    let station: Address = vault.station.parse().unwrap_or_default();
    let on_venue = views.read(
        deployments.venue,
        ILocalVenue::accountValueCall { master: station },
    )?;
    let held = views.read(
        deployments.venue,
        ILocalVenue::marketsHeldByCall { master: station },
    )?;
    let share = if vault.value > 0.0 {
        (dollars(on_venue) / vault.value).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Ok(vec![Leg {
        venue: "Local venue".to_string(),
        chain: vault.chain,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: share,
        valuation: Valuation::OnChain {
            source: "the venue's own account reads".to_string(),
        },
        account: address_of(station),
        key: "an agent the vault's station approved, trade-only".to_string(),
        can: vec![
            "place and cancel orders".to_string(),
            format!(
                "hold positions in {}",
                if held.is_empty() {
                    "any listed market".to_string()
                } else {
                    held.iter()
                        .map(|id| name_of(*id))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        ],
        cannot: vec![
            "withdraw anything".to_string(),
            "approve another key".to_string(),
            "send a dollar anywhere but the vault's own accounts".to_string(),
        ],
        status: VenueStatus::Ok,
        note: None,
    }])
}

/// A strategy's one review, from the chain, with the rest of its report from
/// the store. Refusals have no vault, so they are read with the job instead.
pub fn read_review(
    views: &Views,
    deployments: &Deployments,
    store: &Store,
    strategy_id: U256,
) -> Result<Option<Review>> {
    let entry = views.read(
        deployments.reviews,
        IReviewRegistry::reviewOfCall {
            strategyId: strategy_id,
        },
    )?;
    let signed = entry.review;
    if signed.reviewedAt == 0 {
        return Ok(None);
    }
    let report = store.report(&hash_of(signed.reportHash));
    let missing = || "The report is not in the store.".to_string();
    Ok(Some(Review {
        ratings: signed
            .ratings
            .map(|code| Rating::from_code(code).unwrap_or(Rating::CouldntTell)),
        findings: report
            .as_ref()
            .map(|report| report.findings.clone())
            .unwrap_or_default(),
        summary: report
            .as_ref()
            .map(|report| report.summary.clone())
            .unwrap_or_else(missing),
        reviewed: day(signed.reviewedAt),
        refused: entry.refused,
        sealed: report.as_ref().is_some_and(|report| report.sealed),
        reviewer_image: hash_of(signed.reviewerImage),
        model_id_hash: hash_of(signed.modelIdHash),
        prompt_hash: hash_of(signed.promptHash),
        report_hash: hash_of(signed.reportHash),
        limits: report
            .map(|report| report.limits)
            .unwrap_or_else(|| vec![missing()]),
        recorded_tx: None,
    }))
}

fn read_attestation(
    views: &Views,
    deployments: &Deployments,
    history: &History,
    address: Address,
    created_in_known: bool,
    now: u64,
) -> Result<Option<Attestation>> {
    // The newest few blessings, as the registry's logs have been read so
    // far: none while the block the vault was created in isn't known.
    let mut keys = Vec::new();
    if created_in_known && let Some(blessings) = history.blessings.get(&address) {
        for blessing in blessings.iter().rev() {
            if blessing.until < now {
                continue;
            }
            keys.push(BlessedKey {
                address: address_of(blessing.signer),
                zone: "this machine".to_string(),
                blessed_until: moment(blessing.until),
                proof_tx: blessing.proof_tx.clone(),
                role: KeyRole::Primary,
            });
        }
    }
    if keys.is_empty() {
        return Ok(None);
    }

    let image = views.read(
        deployments.attestation,
        IAttestationRegistry::imageOfCall {
            imageDigest: alloy::primitives::keccak256("shonin-dev-image:executor"),
        },
    )?;
    let builder = views.read(
        deployments.attestation,
        IAttestationRegistry::imageOfCall {
            imageDigest: alloy::primitives::keccak256("shonin-dev-image:builder"),
        },
    )?;
    let ttl = views.read(
        deployments.attestation,
        IAttestationRegistry::blessingTtlCall {},
    )?;

    Ok(Some(Attestation {
        image: Image {
            digest: hash_of(alloy::primitives::keccak256("shonin-dev-image:executor")),
            allowlisted: day(image.allowedAt as u64),
            kind: "executor".to_string(),
        },
        keys,
        approvals: vec![Approval {
            venue: "Local venue".to_string(),
            detail: format!("agent on the vault's account {}", address_of(address)),
            custody: Custody::VenueEnforced,
        }],
        builder: Image {
            digest: hash_of(alloy::primitives::keccak256("shonin-dev-image:builder")),
            allowlisted: day(builder.allowedAt as u64),
            kind: "builder".to_string(),
        },
        trust_root: "dev-local".to_string(),
        blessing_ttl_hours: (ttl / 3_600) as u32,
    }))
}

fn read_bundle(
    store: &Store,
    registered: &chain::bindings::strategies::Strategy,
    bundle_hash: B256,
) -> Bundle {
    bundle_from(
        store,
        registered.manifestHash,
        registered.sourceHash,
        bundle_hash,
    )
}

/// A bundle as its registration names it, with what the store holds of it.
pub(crate) fn bundle_from(
    store: &Store,
    manifest_hash: B256,
    source_hash: B256,
    bundle_hash: B256,
) -> Bundle {
    let hash = hash_of(bundle_hash);
    let canonical = store.manifest(&hash_of(manifest_hash));
    Bundle {
        bundle_hash: hash.clone(),
        component_hash: store
            .component_hash(&hash)
            .unwrap_or_else(|| "not in the store".to_string()),
        source_hash: hash_of(source_hash),
        manifest_hash: hash_of(manifest_hash),
        world: canonical
            .as_ref()
            .map(|canonical| canonical.world.clone())
            .unwrap_or_else(|| "shonin:agent@0.1.0".to_string()),
        toolchain: store
            .toolchain(&hash)
            .unwrap_or_else(|| "not in the store".to_string()),
        builder_image: canonical
            .map(|canonical| canonical.builder_image)
            .unwrap_or_else(|| hash_of(alloy::primitives::keccak256("shonin-dev-image:builder"))),
        models: Vec::<ModelFile>::new(),
        source: store.source(&hash_of(source_hash)),
        source_files: store.source_files(&hash_of(source_hash)),
        built_in_seconds: store.built_in_seconds(&hash),
    }
}

/// What to call an author: the handle they chose, or their address if they
/// chose none. Nothing depends on it either way.
fn handle_of(views: &Views, deployments: &Deployments, author: Address) -> String {
    match views.read(
        deployments.profiles,
        IAuthorProfiles::profileOfCall { author },
    ) {
        Ok(profile) if !profile.handle.is_empty() => profile.handle,
        _ => address_of(author),
    }
}

/// Everyone who has published, each with their address, which is who they
/// are: the handle is only what they are called (see the indexer's
/// `shown_names`), and two authors are never one for sharing it.
pub fn authors(views: &Views, deployments: &Deployments) -> Result<Vec<(Address, Author)>> {
    let mut out: Vec<(Address, Author)> = Vec::new();
    let owners = views.read(deployments.factory, IVaultFactory::vaultsCall {})?;
    let mut seen: Vec<Address> = Vec::new();
    for vault in owners {
        let address = views.read(vault, IVault::authorCall {})?;
        if seen.contains(&address) {
            continue;
        }
        seen.push(address);
        let profile = views.read(
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author: address },
        )?;
        let handle = if profile.handle.is_empty() {
            address_of(address)
        } else {
            profile.handle.clone()
        };
        out.push((
            address,
            Author {
                handle,
                joined: day(profile.joinedAt),
                blurb: if profile.blurb.is_empty() {
                    "This author has not said anything about themselves.".to_string()
                } else {
                    profile.blurb
                },
                links: profile
                    .linkLabels
                    .into_iter()
                    .zip(profile.linkUrls)
                    .collect(),
            },
        ));
    }
    Ok(out)
}

/// What is worth announcing, newest first: the registry's and the
/// allowlist's logs as they have been read so far, and what the vaults show.
pub fn feed(vault_chain: Chain, history: &History, strategies: &[Strategy]) -> Vec<FeedEvent> {
    let mut out = Vec::new();

    for registration in &history.registrations {
        let id = registration.strategy_id;
        out.push(FeedEvent {
            at: moment(registration.at),
            kind: FeedKind::StrategyRegistered,
            title: format!("Agent {id} on {} was registered", vault_chain.label()),
            detail: "The reviewer read it first, then an allowlisted build service compiled it and signed what it made."
                .to_string(),
            link: strategies
                .iter()
                .any(|strategy| strategy.id == id && !never_read(strategy))
                .then(|| format!("/agents/{}", Strategy::slug_for(vault_chain, id))),
            reference: hash_of(registration.bundle_hash),
        });
    }

    for (at, digest) in &history.images {
        out.push(FeedEvent {
            at: moment(*at),
            kind: FeedKind::ImageProposed,
            title: "An image was proposed for the allowlist".to_string(),
            detail: "It becomes blessable when its timelock runs out, and not before.".to_string(),
            link: Some("/verify".to_string()),
            reference: hash_of(*digest),
        });
    }

    for strategy in strategies {
        if let Some(kill) = &strategy.kill {
            out.push(FeedEvent {
                at: kill.at,
                kind: FeedKind::Kill,
                title: format!("{} was stopped", strategy.title),
                detail: kill.outcome.clone(),
                link: Some(format!("/agents/{}", strategy.slug)),
                reference: kill.reference.clone(),
            });
        }
        if strategy.vault.state == VaultState::Frozen {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::VaultFrozen,
                title: format!("{} froze", strategy.title),
                detail: "A claim went unserved, so no new deposits and only unwinding is left."
                    .to_string(),
                link: Some(format!("/agents/{}", strategy.slug)),
                reference: strategy.vault.address.clone(),
            });
        }
        if let Some(review) = &strategy.review {
            out.push(FeedEvent {
                at: moment(0),
                kind: FeedKind::ReviewPublished,
                title: format!("{} was reviewed", strategy.title),
                detail: if review.is_alarming() {
                    "Its report card has serious concerns.".to_string()
                } else {
                    "Its report card is published.".to_string()
                },
                link: Some(format!("/agents/{}/review", strategy.slug)),
                reference: review.report_hash.clone(),
            });
        }
    }

    out.sort_by_key(|event| std::cmp::Reverse(event.at));
    out
}

/// The terms the platform is running under, asked of the contracts that
/// enforce them. They are governed rather than fixed, so the app is told them
/// instead of being built with them.
pub fn terms(views: &Views, deployments: &Deployments) -> Result<shared::Terms> {
    const DAY: f64 = 86_400.0;

    let vaults = views.read(deployments.factory, IVaultFactory::configCall {})?;
    let timelock = views.read(
        deployments.attestation,
        IAttestationRegistry::timelockCall {},
    )?;
    let publish_fee = views.read(deployments.publishing, IPublishing::feeCall {})?;
    let terms_hash = views.read(deployments.factory, IVaultFactory::termsCall {})?;

    Ok(shared::Terms {
        minimum_tank: dollars(vaults.minimumTank),
        publish_fee: dollars(publish_fee),
        fees: shared::Fees {
            holding_bps: vaults.holdingBps as u32,
            author_bps: vaults.authorBps as u32,
            platform_bps: vaults.platformBps as u32,
            transaction_bps: vaults.transactionBps as u32,
        },
        exit_band_bps: vaults.exitBandBps as u32,
        freeze_after_days: vaults.timers.freezeAfter as f64 / DAY,
        image_timelock_days: timelock as f64 / DAY,
        minimum_deposit: dollars(vaults.minimumDeposit),
        terms_hash: format!("{terms_hash:?}"),
    })
}

/// How many payments the publishing contract has taken.
pub fn payments(views: &Views, deployments: &Deployments) -> Result<u64> {
    Ok(views
        .read(deployments.publishing, IPublishing::countCall {})?
        .try_into()
        .unwrap_or(u64::MAX))
}

/// What the chain is asked to complete a job: its author's profile, its
/// strategy's review, and the vault the factory made for it.
pub fn job_reads(deployments: &Deployments, job: &Job) -> Vec<(Address, alloy::primitives::Bytes)> {
    use alloy::sol_types::SolCall;
    let mut reads = Vec::new();
    if let Ok(author) = job.author.parse::<Address>() {
        reads.push((
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author }
                .abi_encode()
                .into(),
        ));
    }
    if let Some(id) = job.strategy_id {
        let strategy_id = U256::from(id);
        reads.push((
            deployments.reviews,
            IReviewRegistry::reviewOfCall {
                strategyId: strategy_id,
            }
            .abi_encode()
            .into(),
        ));
        reads.push((
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id,
            }
            .abi_encode()
            .into(),
        ));
    }
    reads
}

/// A job, as the builder reports it, with what the chain adds: its review
/// (a refusal's included), whether it has been activated, and who its author
/// is called. Any read the chain doesn't answer leaves the job uncompleted.
pub fn job(views: &Views, deployments: &Deployments, store: &Store, mut job: Job) -> Result<Job> {
    if let Ok(author) = job.author.parse::<Address>() {
        let profile = views.read(
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author },
        )?;
        job.author = if profile.handle.is_empty() {
            address_of(author)
        } else {
            profile.handle
        };
    }
    let Some(id) = job.strategy_id else {
        return Ok(job);
    };
    let strategy_id = U256::from(id);
    job.review = read_review(views, deployments, store, strategy_id)?;
    let vault = views.read(
        deployments.factory,
        IVaultFactory::vaultOfCall {
            strategyId: strategy_id,
        },
    )?;
    if vault != Address::ZERO {
        job.state = JobState::Activated;
        job.strategy_slug = Some(Strategy::slug_for(job.chain, id));
    }
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_kept_hourly_for_ninety_days_and_daily_before() {
        let now = 1_800_000_000 - 1_800_000_000 % DAY;
        let reading = |at| Sample {
            at,
            value_per_share: 1.0,
            aum: 0.0,
        };
        // A reading every hour for 120 days.
        let mut samples: Vec<Sample> = (0..120 * 24)
            .rev()
            .map(|hours| reading(now - hours * HOUR))
            .collect();
        prune(&mut samples, now);
        let older = samples
            .iter()
            .filter(|sample| sample.at < now - HOURLY_FOR)
            .count();
        let recent = samples.len() - older;
        assert_eq!(older, 30, "one a day for the thirty days past ninety");
        assert_eq!(
            recent as u64,
            HOURLY_FOR / HOUR + 1,
            "hourly for ninety days"
        );
        assert!(samples.windows(2).all(|pair| pair[0].at < pair[1].at));
    }

    #[test]
    fn the_block_a_time_falls_in_is_guessed_within_the_blocks_between() {
        let low = Mark {
            block: 100,
            time: 1_000,
        };
        let high = Mark {
            block: 200,
            time: 1_200,
        };
        assert_eq!(blocks_until(low, high, 1_100), 50);
        assert_eq!(blocks_until(low, high, 1_001), 1);
        assert_eq!(blocks_until(low, high, 1_000), 1);
        assert_eq!(blocks_until(low, high, 9_999), 100);
    }

    // A vault's log, as it is folded into its holders' costs ---------------

    const VAULT: Address = Address::repeat_byte(0x11);
    const HOLDER: Address = Address::repeat_byte(0x22);
    const SOMEONE_ELSE: Address = Address::repeat_byte(0x33);

    /// One of the vault's events, in transaction `tx` at `index`.
    fn logged(event: impl alloy::sol_types::SolEvent, tx: u8, index: u64) -> Log {
        Log {
            inner: alloy::primitives::Log {
                address: VAULT,
                data: event.encode_log_data(),
            },
            block_hash: Some(B256::repeat_byte(tx)),
            block_number: Some(tx as u64),
            block_timestamp: None,
            transaction_hash: Some(B256::repeat_byte(tx)),
            transaction_index: Some(0),
            log_index: Some(index),
            removed: false,
        }
    }

    fn usd(dollars: u64) -> U256 {
        U256::from(dollars * 1_000_000)
    }

    fn shares_of(count: u64) -> U256 {
        U256::from(count) * U256::from(10u64.pow(18))
    }

    fn deposited(owner: Address, dollars: u64, tx: u8) -> Log {
        logged(
            IVault::Deposited {
                requestId: U256::from(tx),
                owner,
                assets: usd(dollars),
                shares: shares_of(dollars),
            },
            tx,
            0,
        )
    }

    /// The costs `logs` fold to, folded in batches cut at every block
    /// boundary as pages would cut them, and all at once: both the same.
    fn cost_of(logs: &[Log], who: Address) -> f64 {
        let deployments = crate::evm::tests::deployments();
        let mut whole = History::default();
        whole.fold(&deployments, logs);
        let mut paged = History::default();
        for block in logs
            .iter()
            .filter_map(|log| log.block_number)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let page: Vec<Log> = logs
                .iter()
                .filter(|log| log.block_number == Some(block))
                .cloned()
                .collect();
            paged.fold(&deployments, &page);
        }
        assert_eq!(whole.cost(&VAULT, &who), paged.cost(&VAULT, &who));
        whole.cost(&VAULT, &who)
    }

    #[test]
    fn a_claim_paid_once_the_vault_closed_comes_off_its_owner_s_cost_once() {
        let logs = vec![
            deposited(HOLDER, 100, 1),
            deposited(SOMEONE_ELSE, 50, 2),
            // A claim paid while the vault ran: its `Withdrawn` and its
            // `ClaimPaid`, one after the other.
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(8),
                    owner: HOLDER,
                    shares: shares_of(20),
                },
                3,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::from(4),
                    owner: HOLDER,
                    shares: shares_of(20),
                    assets: usd(20),
                    feeShares: U256::ZERO,
                },
                4,
                3,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(8),
                    assets: usd(20),
                },
                4,
                4,
            ),
            // Two claims paid once the vault closed (`claimClosed`): a burn,
            // then the `ClaimPaid` alone.
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(9),
                    owner: HOLDER,
                    shares: shares_of(30),
                },
                5,
                0,
            ),
            logged(
                IVault::ClaimBooked {
                    claimId: U256::from(10),
                    owner: SOMEONE_ELSE,
                    shares: shares_of(10),
                },
                5,
                1,
            ),
            logged(
                IVault::Transfer {
                    from: VAULT,
                    to: Address::ZERO,
                    value: shares_of(30),
                },
                6,
                0,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(9),
                    assets: usd(30),
                },
                6,
                2,
            ),
            logged(
                IVault::Transfer {
                    from: VAULT,
                    to: Address::ZERO,
                    value: shares_of(10),
                },
                7,
                0,
            ),
            logged(
                IVault::ClaimPaid {
                    claimId: U256::from(10),
                    assets: usd(10),
                },
                7,
                2,
            ),
        ];
        assert_eq!(cost_of(&logs, HOLDER), 50.0);
        assert_eq!(cost_of(&logs, SOMEONE_ELSE), 40.0);
    }

    #[test]
    fn a_closed_vault_s_redemption_comes_off_the_holder_s_cost_not_the_receiver_s() {
        let logs = vec![
            deposited(HOLDER, 100, 1),
            deposited(SOMEONE_ELSE, 50, 2),
            // `redeemClosed(5 shares, someone else)`, sent by the holder: the
            // holder's shares burnt, the dollars to the receiver.
            logged(
                IVault::Transfer {
                    from: HOLDER,
                    to: Address::ZERO,
                    value: shares_of(5),
                },
                3,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::ZERO,
                    owner: SOMEONE_ELSE,
                    shares: shares_of(5),
                    assets: usd(40),
                    feeShares: U256::ZERO,
                },
                3,
                2,
            ),
            // And one the receiver redeems for themselves.
            logged(
                IVault::Transfer {
                    from: SOMEONE_ELSE,
                    to: Address::ZERO,
                    value: shares_of(1),
                },
                4,
                0,
            ),
            logged(
                IVault::Withdrawn {
                    requestId: U256::ZERO,
                    owner: SOMEONE_ELSE,
                    shares: shares_of(1),
                    assets: usd(8),
                    feeShares: U256::ZERO,
                },
                4,
                2,
            ),
        ];
        assert_eq!(cost_of(&logs, HOLDER), 60.0);
        assert_eq!(cost_of(&logs, SOMEONE_ELSE), 42.0);
    }
}
