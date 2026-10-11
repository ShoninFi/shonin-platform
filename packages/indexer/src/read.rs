//! Turning the chain into what the site shows.
//!
//! Everything here is read from contracts and logs. A vault's value per share
//! is asked of the vault itself at each past block, so the record is not a
//! store anyone has to trust: delete this service's memory and it rebuilds
//! the same numbers from the chain.

use std::collections::{HashMap, VecDeque};

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::{Filter, Log};
use alloy::sol_types::SolEvent;
use anyhow::Result;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::IAuthorProfiles;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::station::IStationVault;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{
    ClaimState as ChainClaimState, IVault, RequestKind as ChainRequestKind,
    VaultState as ChainVaultState,
};
use chain::bindings::venue::ILocalVenue;
use chain::{Client, Deployments};
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

use crate::logs::{Cursor, read_on};
use crate::store::Store;

/// What the indexer remembers between reads of a chain: the samples it has
/// taken, and what it has made of each stream of logs and how far each has
/// been read.
#[derive(Default)]
pub struct History {
    pub(crate) anchor: Option<(u64, B256)>,
    pub(crate) discard_previous: bool,
    proofs: chain::sealed_views::Proofs,
    /// Each vault's readings, oldest first: hourly for the last
    /// [`HOURLY_FOR`], daily before that.
    samples: HashMap<Address, Vec<Sample>>,
    /// The block and time of each vault's last reading: where the next one
    /// is looked for from.
    sampled: HashMap<Address, Mark>,
    /// An unfinished search, resumed within the next refresh's read budget.
    searches: HashMap<Address, SampleSearch>,
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
    /// What is known of each vault's requests: where its waiting ones start.
    requests: HashMap<Address, Requests>,
    /// Each vault as last read whole, and the chain time it was: what is
    /// shown while a later read of it fails part-way.
    last_whole: HashMap<Address, (Strategy, u64)>,
    /// The factory's `Activated` logs: the block each vault was created in.
    activated: Stream<HashMap<Address, u64>>,
    /// The registry's registrations, for the feed.
    registrations: Stream<Vec<Registration>>,
    /// The images proposed for the allowlist, for the feed.
    images: Stream<Vec<(u64, B256)>>,
    /// Each vault's own logs.
    vaults: HashMap<Address, Stream<VaultLogs>>,
    /// Each vault's blessings.
    blessings: HashMap<Address, Stream<VecDeque<Blessing>>>,
    /// Each station's fills: the vault never trades itself, its home station
    /// does.
    fills: HashMap<Address, Stream<Fills>>,
}

impl History {
    pub(crate) fn reset_branch(&mut self) {
        *self = Self {
            discard_previous: true,
            ..Self::default()
        };
    }

    /// Addresses already discovered, plus the registries whose streams are
    /// followed. A newly discovered address remains on ordinary log reads
    /// until the next observation, so discovery never depends on this list.
    pub(crate) fn log_addresses(&self, deployments: &Deployments) -> Vec<Address> {
        let mut addresses = vec![
            deployments.factory,
            deployments.strategies,
            deployments.attestation,
            deployments.publishing,
            deployments.reviews,
            deployments.profiles,
            deployments.station_factory,
        ];
        addresses.extend(self.vaults.keys().copied());
        addresses.extend(self.blessings.keys().copied());
        addresses.extend(self.fills.keys().copied());
        addresses.sort_unstable();
        addresses.dedup();
        addresses
    }
}

/// One stream of logs: what has been made of it, and how far it has been
/// read.
#[derive(Default)]
struct Stream<T> {
    cursor: Option<Cursor>,
    made: T,
}

impl<T> Stream<T> {
    /// Read whatever is new up to `head`, from `from` the first time, and
    /// fold each log taken into what has been made of the stream. Whether it
    /// is read to the head; a stream that can't be read is said so in the
    /// log and keeps what it had.
    async fn read(
        &mut self,
        chain: &Client,
        filter: &Filter,
        from: u64,
        head: u64,
        what: &str,
        mut fold: impl FnMut(&mut T, &Log),
    ) -> bool {
        let cursor = self.cursor.get_or_insert(Cursor::from(from));
        let pages = read_on(chain, filter, cursor, head).await;
        for log in &pages.logs {
            fold(&mut self.made, log);
        }
        if let Some(error) = pages.error {
            tracing::warn!(%error, "could not read {what}; kept what was read before");
        }
        cursor.caught_up(head)
    }
}

/// A block and its time.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Mark {
    block: u64,
    time: u64,
}

#[derive(Clone, Copy)]
struct SampleSearch {
    origin: Mark,
    due: u64,
    low: Mark,
    high: Mark,
    halve: bool,
}

/// One reading of a vault, dated by its block's time.
#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub(crate) at: u64,
    pub(crate) value_per_share: f64,
    pub(crate) aum: f64,
}

/// A registration, as the feed tells of it.
struct Registration {
    at: u64,
    strategy_id: u64,
    bundle_hash: B256,
}

/// What a vault's own logs come to.
#[derive(Default)]
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
}

/// Where a claim's `ClaimPaid` lies: its block, and its place in the block.
#[derive(Clone, Copy)]
struct Paid {
    block: u64,
    log_index: u64,
}

/// A blessing of a key for a vault.
struct Blessing {
    signer: Address,
    until: u64,
    proof_tx: String,
}

/// A station's fills: how many, and the newest in full.
#[derive(Default)]
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

/// Every vault, as the site shows it. `vault_chain` is the chain these
/// vaults live on, by the name a manifest gives it.
#[allow(clippy::too_many_arguments)]
pub async fn strategies(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    head: u64,
    now: u64,
) -> Result<Vec<Strategy>> {
    let addresses = chain
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;

    // Where each vault was created, so its record starts there: until the
    // factory's log has been read through, a vault not found in it yet may
    // still be.
    let activations = history
        .activated
        .read(
            chain,
            &Filter::new()
                .address(deployments.factory)
                .event_signature(IVaultFactory::Activated::SIGNATURE_HASH),
            deployments.deployed_block,
            head,
            "the vaults' activations",
            |made, log| {
                if let (Ok(activated), Some(block)) = (
                    log.log_decode::<IVaultFactory::Activated>(),
                    log.block_number,
                ) {
                    made.insert(activated.inner.data.vault, block);
                }
            },
        )
        .await;

    let mut out = Vec::new();
    for address in &addresses {
        let created_in = match history.activated.made.get(address) {
            Some(block) => Some((*block).max(deployments.deployed_block)),
            // Read through and not there: no vault existed before the
            // platform did.
            None if activations => Some(deployments.deployed_block),
            None => None,
        };
        match one(
            chain,
            vault_chain,
            deployments,
            store,
            history,
            health,
            *address,
            created_in,
            head,
            now,
        )
        .await
        {
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
                    out.push(unread(chain, vault_chain, deployments, *address).await);
                }
            },
        }
    }

    Ok(out)
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
async fn unread(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    address: Address,
) -> Strategy {
    let id: u64 = match chain.read(address, IVault::strategyIdCall {}).await {
        Ok(id) => id.try_into().unwrap_or_default(),
        Err(_) => 0,
    };
    let (author, author_address) = match chain.read(address, IVault::authorCall {}).await {
        Ok(author) => (
            handle_of(chain, deployments, author).await,
            address_of(author),
        ),
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
async fn one(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    store: &Store,
    history: &mut History,
    health: &HashMap<Address, shared::model::health::Health>,
    address: Address,
    created_in: Option<u64>,
    head: u64,
    now: u64,
) -> Result<Strategy> {
    let views = match history.anchor {
        Some((head, hash)) => chain::sealed_views::SealedViews::observe(
            chain,
            address,
            (head, now, hash),
            &mut history.proofs,
        )
        .await
        .unwrap_or_else(|error| {
            tracing::debug!(%address, error = format!("{error:#}"), "the storage observation is unavailable; views are read live");
            Default::default()
        }),
        None => Default::default(),
    };
    let strategy_id = views
        .read(chain, address, IVault::strategyIdCall {})
        .await?;
    let id: u64 = strategy_id.try_into().unwrap_or_default();
    let author = views.read(chain, address, IVault::authorCall {}).await?;
    let created = views.read(chain, address, IVault::createdAtCall {}).await?;
    let state = views.read(chain, address, IVault::stateCall {}).await?;
    let bundle_hash = views
        .read(chain, address, IVault::bundleHashCall {})
        .await?;
    let registered = chain
        .read(
            deployments.strategies,
            IStrategyRegistry::strategyOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;

    let slug = Strategy::slug_for(vault_chain, id);
    let days_live = (now.saturating_sub(created as u64)) / 86_400;

    let events = read_events(chain, history, address, created_in, head).await;
    let vault = read_vault(chain, vault_chain, history, address, &state, &views).await?;
    let station: Address = vault.station.parse().unwrap_or_default();
    let (trades, trade_count) =
        read_trades(chain, deployments, history, station, created_in, head).await;
    sample(chain, address, created_in, history, head, now).await;
    let record = build_record(history, address, &vault, now, trade_count);
    let legs = read_legs(chain, deployments, &vault).await?;
    let review = read_review(chain, deployments, store, strategy_id).await?;
    let attestation =
        read_attestation(chain, deployments, history, address, created_in, head, now).await?;
    let bundle = read_bundle(store, &registered, bundle_hash);

    let kill = history
        .vaults
        .get(&address)
        .and_then(|logs| logs.made.killed.as_ref())
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
        author: handle_of(chain, deployments, author).await,
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

async fn read_vault(
    chain: &Client,
    vault_chain: Chain,
    history: &mut History,
    address: Address,
    state: &ChainVaultState,
    views: &chain::sealed_views::SealedViews,
) -> Result<Vault> {
    let fees = views.read(chain, address, IVault::feesCall {}).await?;
    let tank = views.read(chain, address, IVault::tankCall {}).await?;
    let value = views
        .read(chain, address, IVault::totalValueCall {})
        .await?;
    let cash = views.read(chain, address, IVault::cashCall {}).await?;
    let escrow = views.read(chain, address, IVault::escrowCall {}).await?;
    let share_price = views
        .read(chain, address, IVault::sharePriceCall {})
        .await?;
    let mark = views.read(chain, address, IVault::markCall {}).await?;
    let last_payment = views
        .read(chain, address, IVault::lastPaymentCall {})
        .await?;
    let created = views.read(chain, address, IVault::createdAtCall {}).await?;
    let timers = views.read(chain, address, IVault::timersCall {}).await?;
    let exit_band = views
        .read(chain, address, IVault::exitBandBpsCall {})
        .await?;
    let station = views
        .read(chain, address, IVault::homeStationCall {})
        .await?;
    let floor = views.read(chain, address, IVault::idleFloorCall {}).await?;
    let allowances = views
        .read(chain, address, IVault::allowancesCall {})
        .await?;
    let restricted = views
        .read(chain, address, IVault::restrictedCall {})
        .await?;
    let halted = views
        .read(chain, address, IStationVault::haltedAtCall {})
        .await?;
    let supply = views
        .read(chain, address, IVault::totalSupplyCall {})
        .await?;

    let mut claims = Vec::new();
    let count: u64 = chain
        .read(address, IVault::claimCountCall {})
        .await?
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
        let claim = chain
            .read(
                address,
                IVault::claimAtCall {
                    claimId: U256::from(id),
                },
            )
            .await?;
        let (fraction, measured) = if matches!(claim.state, ChainClaimState::Paid) {
            closed_by_paid(chain, history, address, id, claim.shares, supply).await
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
    let requests = read_requests(chain, history, address).await?;

    let daily = shared::Prices::published().daily_cap(&shared_allowances(&allowances));
    Ok(Vault {
        address: address_of(address),
        chain: vault_chain,
        asset: vault_chain.dollar().to_string(),
        state: match state {
            ChainVaultState::Activating => VaultState::Activating,
            ChainVaultState::Open => VaultState::Open,
            ChainVaultState::WindingDown => VaultState::WindingDown,
            ChainVaultState::Frozen => VaultState::Frozen,
            ChainVaultState::Killed => VaultState::Killed,
            ChainVaultState::Cancelled => VaultState::Cancelled,
            _ => VaultState::Closed,
        },
        created: day(created),
        station: address_of(station),
        value: dollars(value),
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
/// its payment is in the vault's log. Until then (the log not read that far
/// yet, or the supply then not answered) it is the share it stood for when
/// last read waiting, or today's supply with its own shares put back, and is
/// worked out again at the next refresh; once the supply then has gone
/// unanswered [`UNANSWERED_TRIES`] refreshes in a row (a node that keeps no
/// state that far back), that share is kept for good, so a claim no node
/// will measure isn't read again at every refresh for ever.
async fn closed_by_paid(
    chain: &Client,
    history: &mut History,
    vault: Address,
    id: u64,
    claimed: U256,
    supply: U256,
) -> (f64, bool) {
    let paid_in = history.vaults.get(&vault).map(|logs| &logs.made.paid_in);
    let mut given_up = false;
    if let Some(paid) = paid_in.and_then(|paid_in| paid_in.get(&id)) {
        match supply_before(chain, vault, *paid, paid_in.into_iter().flatten()).await {
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
/// before, less the shares of every claim paid ahead of it in its block.
async fn supply_before<'a>(
    chain: &Client,
    vault: Address,
    paid: Paid,
    every: impl Iterator<Item = (&'a u64, &'a Paid)>,
) -> Result<U256> {
    let mut supply = chain
        .read_at(
            vault,
            IVault::totalSupplyCall {},
            BlockId::number(paid.block.saturating_sub(1)),
        )
        .await?;
    for (earlier, _) in
        every.filter(|(_, other)| other.block == paid.block && other.log_index < paid.log_index)
    {
        let claim = chain
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(*earlier),
                },
            )
            .await?;
        supply = supply.saturating_sub(claim.shares);
    }
    Ok(supply)
}

/// What is known of a vault's requests from one refresh to the next: a
/// request once done stays done, so it is not read again.
#[derive(Default)]
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
async fn read_requests(
    chain: &Client,
    history: &mut History,
    address: Address,
) -> Result<Vec<Request>> {
    let count: u64 = chain
        .read(address, IVault::requestCountCall {})
        .await?
        .try_into()
        .unwrap_or(0);
    let known = history.requests.entry(address).or_default();
    known.from = known.from.max(1);
    let read = waiting_requests(chain, known, address, count).await;
    // Every request before the first not done is done.
    while known.done.remove(&known.from) {
        known.from += 1;
    }
    read
}

async fn waiting_requests(
    chain: &Client,
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
        let request = chain
            .read(
                address,
                IVault::requestAtCall {
                    requestId: U256::from(id),
                },
            )
            .await?;
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

/// Take the vault's readings due since the last: one an hour of chain time
/// (one a day for what is older than [`HOURLY_FOR`]), from the block it was
/// created in on. Each reading is taken at the first block whose time is
/// due, searched for between the last reading's block and the head: guessed
/// from the pace the chain kept between the two ends, and halved instead
/// whenever a guess fails to halve what is left, so a chain whose pace
/// changes is searched in as many reads as halving would take. A refresh
/// asks for at most the configured history_page_reads block times, and the next carries
/// on from where it stopped. A reading whose block's time can't be read, or
/// that the vault doesn't answer, is not taken: it is asked for again at the
/// next refresh, never dated by a guess. `created_in` is `None` while the
/// block the vault was created in isn't known yet.
async fn sample(
    chain: &Client,
    address: Address,
    created_in: Option<u64>,
    history: &mut History,
    head: u64,
    now: u64,
) {
    sample_with_budget(
        chain,
        address,
        created_in,
        history,
        head,
        now,
        chain.rpc_policy().history_page_reads,
    )
    .await;
}

async fn sample_with_budget(
    chain: &Client,
    address: Address,
    created_in: Option<u64>,
    history: &mut History,
    head: u64,
    now: u64,
    limit: u64,
) {
    let mut reads = 0;
    let mut last = match history.sampled.get(&address) {
        Some(last) => *last,
        None => {
            let Some(start) = created_in else {
                return;
            };
            let block = start.clamp(1, head.max(1));
            reads += 1;
            let Ok(time) = block_time(chain, block).await else {
                return;
            };
            match take(chain, history, address, Mark { block, time }).await {
                Some(taken) => taken,
                None => return,
            }
        }
    };

    'readings: while reads < limit {
        let spacing = if now.saturating_sub(last.time) > HOURLY_FOR {
            DAY
        } else {
            HOUR
        };
        let due = last.time.saturating_add(spacing);
        if due > now || last.block >= head {
            break;
        }
        // The first block at or after `due` lies past `low`, whose time is
        // before it, and no further than `high`, whose time is not.
        let search = history.searches.remove(&address).filter(|search| {
            search.origin == last && search.due == due && search.high.block <= head
        });
        let (mut low, mut high, mut halve) = search.map_or(
            (
                last,
                Mark {
                    block: head,
                    time: now,
                },
                false,
            ),
            |search| (search.low, search.high, search.halve),
        );
        while high.block - low.block > 1 {
            if reads >= limit {
                history.searches.insert(
                    address,
                    SampleSearch {
                        origin: last,
                        due,
                        low,
                        high,
                        halve,
                    },
                );
                break 'readings;
            }
            let left = high.block - low.block;
            let ahead = if halve {
                left / 2
            } else {
                blocks_until(low, high, due).min(left - 1)
            };
            reads += 1;
            let Ok(time) = block_time(chain, low.block + ahead).await else {
                history.searches.insert(
                    address,
                    SampleSearch {
                        origin: last,
                        due,
                        low,
                        high,
                        halve,
                    },
                );
                break 'readings;
            };
            let mark = Mark {
                block: low.block + ahead,
                time,
            };
            if time >= due {
                high = mark;
            } else {
                low = mark;
            }
            halve = (high.block - low.block) > left / 2;
        }
        match take(chain, history, address, high).await {
            Some(taken) => last = taken,
            None => {
                history.searches.insert(
                    address,
                    SampleSearch {
                        origin: last,
                        due,
                        low,
                        high,
                        halve,
                    },
                );
                break;
            }
        }
    }
    prune(history.samples.entry(address).or_default(), now);
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
async fn take(chain: &Client, history: &mut History, address: Address, at: Mark) -> Option<Mark> {
    match price_at(chain, address, at.block).await {
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
            tracing::warn!(vault = %address, block = at.block, %error, "a reading the vault never answered was passed over");
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

async fn price_at(chain: &Client, address: Address, block: u64) -> Result<(f64, f64)> {
    let price = chain
        .read_at(address, IVault::sharePriceCall {}, BlockId::number(block))
        .await?;
    let value = chain
        .read_at(address, IVault::totalValueCall {}, BlockId::number(block))
        .await?;
    Ok((dollars(price), dollars(value)))
}

async fn block_time(chain: &Client, block: u64) -> Result<u64> {
    chain.block_timestamp_at(block).await
}

/// Every fill the vault's home station has had (the vault never trades
/// itself: its station is the venue account the agent trades), read on from
/// where the last read stopped: the last few in full, for the page, and how
/// many there were altogether, for the record. Fills that can't be read
/// this time leave what was read before. The station was made with its
/// vault, so they are read from the block the vault was created in, and not
/// at all while that isn't known yet.
async fn read_trades(
    chain: &Client,
    deployments: &Deployments,
    history: &mut History,
    station: Address,
    created_in: Option<u64>,
    head: u64,
) -> (Vec<Trade>, u32) {
    if station == Address::ZERO {
        return (Vec::new(), 0);
    }
    let fills = history.fills.entry(station).or_default();
    if let Some(created_in) = created_in {
        fills
            .read(
                chain,
                &Filter::new()
                    .address(deployments.venue)
                    .event_signature(ILocalVenue::Filled::SIGNATURE_HASH)
                    .topic1(station.into_word()),
                created_in,
                head,
                "a station's fills",
                |made, log| {
                    let Ok(filled) = log.log_decode::<ILocalVenue::Filled>() else {
                        return;
                    };
                    let event = filled.inner.data;
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
                },
            )
            .await;
    }
    let fills = &fills.made;
    (fills.newest.iter().rev().cloned().collect(), fills.count)
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
        .map_or(strategy.trades.len() as u32, |fills| fills.made.count)
}

/// Why the vault halted, from its last `Halted` log, in words.
fn halt_reason(history: &History, address: Address) -> String {
    let Some(reason) = history
        .vaults
        .get(&address)
        .and_then(|logs| logs.made.halted)
    else {
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

/// The vault's events the page shows, newest first, read on from where the
/// last read stopped; and, kept as they are read, why it last halted and
/// where each claim was paid, and when it was last stopped. Logs that can't
/// be read this time leave what was read before. They are read from the
/// block the vault was created in, and not at all while that isn't known
/// yet.
async fn read_events(
    chain: &Client,
    history: &mut History,
    address: Address,
    created_in: Option<u64>,
    head: u64,
) -> Vec<VaultEvent> {
    let logs = history.vaults.entry(address).or_default();
    let Some(created_in) = created_in else {
        return Vec::new();
    };
    logs.read(
        chain,
        &Filter::new().address(address),
        created_in,
        head,
        "a vault's events",
        |made, log| {
            if let Ok(halted) = log.log_decode::<IVault::Halted>() {
                made.halted = Some(halted.inner.data.reason);
                made.halted_in = Some((log.block_number, log.transaction_hash));
            }
            if let (Ok(paid), Some(block), Some(log_index)) = (
                log.log_decode::<IVault::ClaimPaid>(),
                log.block_number,
                log.log_index,
            ) && let Ok(id) = u64::try_from(paid.inner.data.claimId)
            {
                made.paid_in.insert(id, Paid { block, log_index });
            }
            // The newest sixty the feed shows, counted after the logs it
            // doesn't show (share transfers and the like) are left out.
            let event = vault_event(log).or_else(|| killed_after_halt(made, log));
            if let Some(event) = event {
                if event.kind == EventKind::Killed {
                    made.killed = Some(event.clone());
                }
                made.events.push_back(event);
                if made.events.len() > SHOWN {
                    made.events.pop_front();
                }
            }
        },
    )
    .await;
    logs.made.events.iter().rev().cloned().collect()
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

async fn read_legs(chain: &Client, deployments: &Deployments, vault: &Vault) -> Result<Vec<Leg>> {
    let station: Address = vault.station.parse().unwrap_or_default();
    let on_venue = chain
        .read(
            deployments.venue,
            ILocalVenue::accountValueCall { master: station },
        )
        .await?;
    let held = chain
        .read(
            deployments.venue,
            ILocalVenue::marketsHeldByCall { master: station },
        )
        .await?;
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
pub async fn read_review(
    chain: &Client,
    deployments: &Deployments,
    store: &Store,
    strategy_id: U256,
) -> Result<Option<Review>> {
    let entry = chain
        .read(
            deployments.reviews,
            IReviewRegistry::reviewOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
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
    }))
}

async fn read_attestation(
    chain: &Client,
    deployments: &Deployments,
    history: &mut History,
    address: Address,
    created_in: Option<u64>,
    head: u64,
    now: u64,
) -> Result<Option<Attestation>> {
    // The newest few blessings, read on from where the last read stopped,
    // from the block the vault was created in; a read that fails leaves
    // those read before.
    let blessings = history.blessings.entry(address).or_default();
    if let Some(created_in) = created_in {
        blessings
            .read(
                chain,
                &Filter::new()
                    .address(deployments.attestation)
                    .event_signature(IAttestationRegistry::Blessed::SIGNATURE_HASH)
                    .topic2(address.into_word()),
                created_in,
                head,
                "a vault's blessings",
                |made, log| {
                    let Ok(blessed) = log.log_decode::<IAttestationRegistry::Blessed>() else {
                        return;
                    };
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
                },
            )
            .await;
    }

    let mut keys = Vec::new();
    for blessing in blessings.made.iter().rev() {
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
    if keys.is_empty() {
        return Ok(None);
    }

    let image = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: alloy::primitives::keccak256("shonin-dev-image:executor"),
            },
        )
        .await?;
    let builder = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::imageOfCall {
                imageDigest: alloy::primitives::keccak256("shonin-dev-image:builder"),
            },
        )
        .await?;
    let ttl = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessingTtlCall {},
        )
        .await?;

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
async fn handle_of(chain: &Client, deployments: &Deployments, author: Address) -> String {
    match chain
        .read(
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author },
        )
        .await
    {
        Ok(profile) if !profile.handle.is_empty() => profile.handle,
        _ => address_of(author),
    }
}

/// Everyone who has published, each with their address, which is who they
/// are: the handle is only what they are called (see the indexer's
/// `shown_names`), and two authors are never one for sharing it.
pub async fn authors(chain: &Client, deployments: &Deployments) -> Result<Vec<(Address, Author)>> {
    let mut out: Vec<(Address, Author)> = Vec::new();
    let owners = chain
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    let mut seen: Vec<Address> = Vec::new();
    for vault in owners {
        let address = chain.read(vault, IVault::authorCall {}).await?;
        if seen.contains(&address) {
            continue;
        }
        seen.push(address);
        let profile = chain
            .read(
                deployments.profiles,
                IAuthorProfiles::profileOfCall { author: address },
            )
            .await?;
        let handle = if profile.handle.is_empty() {
            address_of(address)
        } else {
            profile.handle.clone()
        };
        out.push((
            address,
            Author {
                handle,
                joined: day(profile.joinedAt as u64),
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

/// What is worth announcing, newest first. The registry's and the
/// allowlist's logs are read on from where the last read stopped; one that
/// can't be read leaves what was read before, and holds up nothing else.
pub async fn feed(
    chain: &Client,
    vault_chain: Chain,
    deployments: &Deployments,
    history: &mut History,
    head: u64,
    strategies: &[Strategy],
) -> Vec<FeedEvent> {
    let mut out = Vec::new();

    history
        .registrations
        .read(
            chain,
            &Filter::new()
                .address(deployments.strategies)
                .event_signature(IStrategyRegistry::Registered::SIGNATURE_HASH),
            deployments.deployed_block,
            head,
            "the registrations",
            |made, log| {
                if let Ok(event) = log.log_decode::<IStrategyRegistry::Registered>() {
                    made.push(Registration {
                        at: log.block_timestamp.unwrap_or_default(),
                        strategy_id: event.inner.strategyId.try_into().unwrap_or_default(),
                        bundle_hash: event.inner.data.bundleHash,
                    });
                }
            },
        )
        .await;
    for registration in &history.registrations.made {
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

    history
        .images
        .read(
            chain,
            &Filter::new()
                .address(deployments.attestation)
                .event_signature(IAttestationRegistry::ImageProposed::SIGNATURE_HASH),
            deployments.deployed_block,
            head,
            "the images proposed",
            |made, log| {
                if let Ok(event) = log.log_decode::<IAttestationRegistry::ImageProposed>() {
                    made.push((
                        log.block_timestamp.unwrap_or_default(),
                        event.inner.imageDigest,
                    ));
                }
            },
        )
        .await;
    for (at, digest) in &history.images.made {
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

/// A job, as the builder reports it, with what the chain adds: its review
/// (a refusal's included), whether it has been activated, and who its author
/// is called. Any read the chain doesn't answer leaves the job uncompleted.
pub async fn job(
    chain: &Client,
    deployments: &Deployments,
    store: &Store,
    mut job: Job,
) -> Result<Job> {
    if let Ok(author) = job.author.parse::<Address>() {
        let profile = chain
            .read(
                deployments.profiles,
                IAuthorProfiles::profileOfCall { author },
            )
            .await?;
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
    job.review = read_review(chain, deployments, store, strategy_id).await?;
    let vault = chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
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
}

#[cfg(test)]
mod sample_budget_tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};

    #[tokio::test]
    async fn a_single_probe_per_refresh_eventually_finds_the_first_due_block() {
        let times = [0, 100, 200, 300, 3_700, 8_000, 12_000, 16_000, 20_000];
        let app = Router::new().route("/", post(move |Json(request): Json<Value>| async move {
            let result = match request["method"].as_str().unwrap_or_default() {
                "eth_chainId" => json!("0x7a69"),
                "eth_blockNumber" => json!("0x8"),
                "eth_getBlockByNumber" => {
                    let number = request["params"][0].as_str()
                        .and_then(|tag| tag.strip_prefix("0x"))
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok()).unwrap_or(8);
                    let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = number;
                    block.header.inner.timestamp = times[number as usize];
                    block.header.hash = B256::repeat_byte(number as u8);
                    serde_json::to_value(block).unwrap()
                }
                "eth_call" => json!(B256::from(U256::from(1_000_000_000_000_000_000u64))),
                _ => return Json(json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32601,"message":"unknown method"}})),
            };
            Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = chain::ChainConfig {
            label: "stand-in".into(),
            rpc: url.clone(),
            public_rpc: url,
            ws: None,
            dollar: "USDC".into(),
            native_usd: None,
            logs_rpc: None,
            log_range: None,
            confirmations: None,
        };
        let client = Client::connect(&config, chain::Signing::ReadOnly)
            .await
            .unwrap();
        let address = Address::repeat_byte(1);
        let mut history = History::default();
        for _ in 0..12 {
            sample_with_budget(&client, address, Some(1), &mut history, 8, 20_000, 1).await;
            if history
                .samples
                .get(&address)
                .is_some_and(|samples| samples.iter().any(|sample| sample.at == 3_700))
            {
                break;
            }
        }
        let samples = &history.samples[&address];
        assert_eq!(samples[0].at, 100);
        assert_eq!(
            samples[1].at, 3_700,
            "the first due reading must finish despite the single-probe budget"
        );
        history.reset_branch();
        assert!(history.searches.is_empty());
        assert!(history.samples.is_empty());
        server.abort();
    }
}
