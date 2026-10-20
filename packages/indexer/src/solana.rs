//! Reading a Solana vault chain into the record.
//!
//! Every vault is an account of the vault program, so the vaults are listed
//! by asking for the program's vault accounts, and each one's state is its
//! account as it is now. A cluster keeps no past state to read a vault at, so
//! its record is rebuilt from its own history instead: every transaction that
//! touched it, read once and in order, and every valuation's figures taken
//! from the event the vault emitted through its own instruction data (which
//! a node can't truncate, as it does logs) under the vault instruction that
//! named it. The same pass keeps the vault's events for the page and what
//! each holder has paid in and taken out.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use chain::SolanaServed;
use chain::solana::platform::{profile_address, publishing_config, review_address};
use chain::solana::programs::anchor_lang::{AccountDeserialize, AnchorDeserialize, Discriminator};
use chain::solana::programs::station::state as station_state;
use chain::solana::programs::{profiles, publishing, vault as sv};
use chain::solana::rpc::Account;
use chain::solana::{Pubkey, SolanaClient, TransactionRecord};
use shared::model::activity::{EventKind, Kill, VaultEvent};
use shared::model::common::Chain;
use shared::model::health::Health;
use shared::model::registry::{Author, FeedEvent, FeedKind, Job, JobState};
use shared::model::review::{Rating, Review};
use shared::model::strategy::{Standing, Strategy};
use shared::model::vault::{
    Claim, ClaimState, Fees, Halt, PendingFloor, Request, RequestKind, Tank, Vault, VaultState,
};
use shared::model::venue::{Custody, ExitClass, ExitPath, Leg, Valuation, VenueStatus};
use shared::{ChainAddress, Terms};
use sv::events as ev;
use sv::state::{
    ClaimState as ChainClaimState, HaltReason, RequestKind as ChainRequestKind,
    VaultState as ChainVaultState,
};

use crate::read::{self, Sample, day, hash_of, moment, name_of};
use crate::store::Store;

/// What the indexer remembers between reads of a Solana chain.
#[derive(Default)]
pub struct History {
    vaults: HashMap<Pubkey, VaultLog>,
    /// Each vault as last read whole, and the chain time it was.
    last_whole: HashMap<Pubkey, (Strategy, u64)>,
    /// The vaults as last listed, by strategy id, and when that was, with
    /// the factory's count of vaults then.
    listed: Vec<Pubkey>,
    listed_at: Option<Instant>,
    listed_count: Option<u64>,
    /// Every account read last time: the next refresh reads them all in
    /// one go.
    planned: BTreeSet<Pubkey>,
    /// What each vault's accounts (the vault, its claims and its list of
    /// restricted places) came to when its requests and history were last
    /// read: they are read again only once that changes.
    digests: HashMap<Pubkey, u64>,
    /// Each vault's requests still waiting, as last listed.
    requests: HashMap<Pubkey, Vec<Request>>,
    /// The slot the last refresh read at: no later read is taken from a
    /// node behind it.
    slot: u64,
}

/// What a vault's history has come to so far, and where reading it stands.
#[derive(Default)]
struct VaultLog {
    /// Transactions listed and not read yet, oldest first.
    queue: VecDeque<String>,
    /// The newest transaction listed: the next listing stops there.
    listed_until: Option<String>,
    /// One reading per valuation, kept hourly then daily as the EVM record.
    samples: Vec<Sample>,
    /// The newest valuation read, by its index.
    last_valuation: Option<u64>,
    /// The newest of what the page shows, oldest first.
    events: VecDeque<VaultEvent>,
    /// The newest time the vault was stopped.
    killed: Option<VaultEvent>,
    /// What each holder has paid in, less what they have taken out.
    paid: HashMap<Pubkey, f64>,
    /// Every claim the vault booked, by its id, as its history tells of it.
    /// A claim's id is the id of the withdrawal it was booked from, not its
    /// place in the vault's count of claims, so this is how the record finds
    /// each claim's account; and a claim the closed vault pays says only its
    /// id, so this is who it is taken off the cost of.
    claims: BTreeMap<u64, Booked>,
    /// Whether the whole history listed has been read.
    read_through: bool,
}

/// A claim as the vault's history tells of it: what is known of it once its
/// account is closed.
#[derive(Clone, Copy)]
struct Booked {
    owner: Pubkey,
    /// The shares it was booked for.
    shares: u128,
    /// When the transaction that booked it landed.
    at: u64,
    /// What it paid, over every part paid.
    paid: u64,
}

/// How many of a vault's transactions are listed per request.
const SIGNATURE_PAGE: usize = 1_000;

/// How many of a vault's events the page shows.
const SHOWN: usize = 60;

/// The most rounds of reads one refresh makes for accounts the ones read
/// name (a vault's claims and station, a key's image).
const ROUNDS: usize = 4;

const HOUR: u64 = 3_600;

/// Share units per share, and dollar units per dollar.
const SHARE: f64 = 1e18;
const DOLLAR: f64 = 1e6;

/// One Solana chain's part of the record.
pub struct ChainRead {
    /// The slot the accounts were read at.
    pub slot: u64,
    /// The cluster's clock then.
    pub now: u64,
    pub strategies: Vec<Strategy>,
    pub authors: Vec<(ChainAddress, Author)>,
    /// The platform's terms on the cluster, if they could be read.
    pub terms: Option<Terms>,
    /// How many payments the publishing program has taken.
    pub payments: Option<u64>,
    /// What each holder paid, by vault, for every vault whose history has
    /// been read through.
    pub costs: HashMap<Pubkey, HashMap<Pubkey, f64>>,
    /// Whether some vault's history is still being read.
    pub catching_up: bool,
    /// Whether each key asked about (by vault and key) is blessed as the
    /// vault's executor, as the accounts read say.
    pub blessed: HashMap<(Pubkey, Pubkey), bool>,
}

/// Accounts read, by address: `None` where there is none.
type Accounts = HashMap<Pubkey, Option<Account>>;

fn data<'a>(accounts: &'a Accounts, address: &Pubkey) -> Option<&'a [u8]> {
    accounts
        .get(address)?
        .as_ref()
        .map(|account| account.data.as_slice())
        .filter(|data| !data.is_empty())
}

fn decode<T: AccountDeserialize>(accounts: &Accounts, address: &Pubkey) -> Option<T> {
    T::try_deserialize(&mut data(accounts, address)?).ok()
}

/// Every vault on the chain, as the site shows it, and their authors, read
/// in one go of every account the record shows (a vault, its claims, its
/// station, its registration, review and author's profile, the platform's
/// settings, the cluster's clock) and the accounts those name, as of one
/// slot or later than the last read. The vaults are listed again only when
/// the factory's count of them changes, a listed one is gone, or
/// `rescan_every` has passed; a vault's requests and history are read again
/// only when its accounts have changed, or while its history is still being
/// read. `asked` are the keys health reports were signed with, by vault.
#[allow(clippy::too_many_arguments)]
pub async fn read(
    served: &SolanaServed,
    store: &Store,
    history: &mut History,
    health: &HashMap<Pubkey, Health>,
    asked: &BTreeSet<(Pubkey, Pubkey)>,
    rescan_every: Duration,
    transactions_per_refresh: usize,
) -> Result<ChainRead> {
    let client = &served.client;
    let mut accounts: Accounts = HashMap::new();
    let mut slot = history.slot;
    let mut listing_checked = false;
    for _ in 0..ROUNDS {
        let mut wanted = wanted(served, history, &accounts, asked);
        if accounts.is_empty() {
            wanted.extend(history.planned.iter().copied());
        }
        let missing: Vec<Pubkey> = wanted
            .into_iter()
            .filter(|address| !accounts.contains_key(address))
            .collect();
        if missing.is_empty() {
            if listing_checked {
                break;
            }
        } else {
            let (at, read) = client
                .rpc
                .accounts_since(&missing, slot)
                .await
                .context("reading the accounts")?;
            slot = slot.max(at);
            accounts.extend(missing.into_iter().zip(read));
        }
        if !listing_checked {
            listing_checked = true;
            if listing_due(history, &accounts, rescan_every) {
                history.listed = vaults(client)
                    .await?
                    .into_iter()
                    .map(|(address, _)| address)
                    .collect();
                history.listed_at = Some(Instant::now());
                history.listed_count =
                    decode::<sv::state::Factory>(&accounts, &sv::state::factory_address())
                        .map(|factory| factory.vault_count);
            }
        }
    }
    history.slot = slot;
    // What the accounts read name: the next refresh reads them all at once
    // (none it no longer needs).
    history.planned = wanted(served, history, &accounts, asked);
    let now = clock_seconds(&accounts)?;

    let mut strategies = Vec::new();
    let mut authors: Vec<(ChainAddress, Author)> = Vec::new();
    let mut costs = HashMap::new();
    let mut catching_up = false;
    let listed = history.listed.clone();
    for address in &listed {
        let Some(vault) = decode::<sv::state::Vault>(&accounts, address) else {
            // Gone since it was listed: listed again at the next refresh.
            continue;
        };
        match one(
            served,
            store,
            history,
            health,
            &accounts,
            *address,
            &vault,
            now,
            transactions_per_refresh,
        )
        .await
        {
            Ok(strategy) => {
                history.last_whole.insert(*address, (strategy.clone(), now));
                strategies.push(strategy);
            }
            Err(error) => match history.last_whole.get(address) {
                Some((strategy, read_at)) => {
                    tracing::warn!(vault = %address, error = format!("{error:#}"), "could not read a vault; kept it as last read");
                    strategies.push(Strategy {
                        stale_as_of: Some(moment(*read_at)),
                        ..strategy.clone()
                    });
                }
                None => {
                    tracing::warn!(vault = %address, error = format!("{error:#}"), "could not read a vault never read whole; shown as not read yet");
                    strategies.push(read::placeholder(
                        served.vault_chain,
                        vault.strategy_id,
                        vault.author.to_string(),
                        vault.author.to_string(),
                        address.to_string(),
                    ));
                }
            },
        }
        // A vault whose history is read through has its holders' costs
        // known; one with transactions still listed to read is catching up.
        match history.vaults.get(address) {
            Some(log) if log.read_through => {
                costs.insert(*address, log.paid.clone());
            }
            Some(log) if !log.queue.is_empty() => catching_up = true,
            _ => {}
        }
        let author = ChainAddress::Solana(vault.author.to_bytes());
        if !authors.iter().any(|(known, _)| *known == author) {
            let profile = decode::<profiles::Profile>(&accounts, &profile_address(&vault.author));
            authors.push((author, author_of(&vault.author, profile)));
        }
    }
    let blessed = asked
        .iter()
        .map(|(vault, signer)| {
            (
                (*vault, *signer),
                blessed_executor(served, &accounts, vault, signer, now),
            )
        })
        .collect();
    Ok(ChainRead {
        slot,
        now,
        terms: terms(&accounts, now),
        payments: decode::<publishing::state::Config>(&accounts, &publishing_config())
            .map(|config| config.count),
        strategies,
        authors,
        costs,
        catching_up,
        blessed,
    })
}

/// Whether the vaults are to be listed again: never listed, the factory's
/// count changed, a listed vault is gone, or `rescan_every` has passed.
fn listing_due(history: &History, accounts: &Accounts, rescan_every: Duration) -> bool {
    let Some(listed_at) = history.listed_at else {
        return true;
    };
    let count = decode::<sv::state::Factory>(accounts, &sv::state::factory_address())
        .map(|factory| factory.vault_count);
    listed_at.elapsed() >= rescan_every
        || count != history.listed_count
        || history
            .listed
            .iter()
            .any(|vault| accounts.get(vault).is_some_and(Option::is_none))
}

/// Every account the record reads, as far as the accounts read so far name
/// them.
fn wanted(
    served: &SolanaServed,
    history: &History,
    accounts: &Accounts,
    asked: &BTreeSet<(Pubkey, Pubkey)>,
) -> BTreeSet<Pubkey> {
    use chain::solana::programs::registry::read as registry;

    let mut wanted = BTreeSet::from([
        chain::solana::programs::anchor_lang::solana_program::sysvar::clock::ID,
        sv::state::factory_address(),
        publishing_config(),
        registry::config_address(),
    ]);
    for address in &history.listed {
        wanted.insert(*address);
        let Some(vault) = decode::<sv::state::Vault>(accounts, address) else {
            continue;
        };
        for at in claim_addresses(history, address) {
            wanted.insert(at);
        }
        wanted.insert(sv::state::restricted_address(address).0);
        wanted.insert(vault.station);
        wanted.insert(publishing::state::strategy_address(vault.strategy_id));
        wanted.insert(review_address(vault.strategy_id));
        wanted.insert(profile_address(&vault.author));
    }
    for (vault, signer) in asked {
        wanted.insert(*vault);
        wanted.insert(registry::key_address(signer));
        if let Some(account) = decode::<sv::state::Vault>(accounts, vault) {
            wanted.insert(registry::blessing_address(
                signer,
                &served.platform.cluster,
                &vault.to_bytes(),
                &account.bundle_hash,
            ));
        }
        if let Some(key) = decode::<chain::solana::programs::registry::state::KeyInfo>(
            accounts,
            &registry::key_address(signer),
        ) {
            wanted.insert(registry::image_address(&key.image_digest));
            wanted.insert(registry::project_address(&key.project_id));
        }
    }
    wanted
}

/// The cluster's clock, from its account.
fn clock_seconds(accounts: &Accounts) -> Result<u64> {
    let clock = data(
        accounts,
        &chain::solana::programs::anchor_lang::solana_program::sysvar::clock::ID,
    )
    .ok_or_else(|| anyhow!("the cluster's clock was not read"))?;
    let bytes: [u8; 8] = clock
        .get(32..40)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| anyhow!("the clock is not a clock"))?;
    Ok(i64::from_le_bytes(bytes).max(0) as u64)
}

/// The terms the cluster's programs enforce, from their accounts.
fn terms(accounts: &Accounts, now: u64) -> Option<Terms> {
    const DAY: f64 = 86_400.0;
    let factory = decode::<sv::state::Factory>(accounts, &sv::state::factory_address())?;
    let publishing = decode::<publishing::state::Config>(accounts, &publishing_config())?;
    let registry = decode::<chain::solana::programs::registry::state::Config>(
        accounts,
        &chain::solana::programs::registry::read::config_address(),
    )?;
    let terms = &factory.terms;
    let dollars = |units: u64| units as f64 / DOLLAR;
    Some(Terms {
        minimum_tank: dollars(terms.minimum_tank),
        publish_fee: dollars(publishing.fee),
        fees: shared::Fees {
            holding_bps: terms.fees.holding_bps as u32,
            author_bps: terms.fees.author_bps as u32,
            platform_bps: terms.fees.platform_bps as u32,
            transaction_bps: terms.fees.transaction_bps as u32,
        },
        exit_band_bps: terms.exit_band_bps as u32,
        freeze_after_days: terms.timers.freeze_after as f64 / DAY,
        image_timelock_days: registry.timelock(now) as f64 / DAY,
        minimum_deposit: dollars(terms.minimum_deposit),
        terms_hash: format!("0x{}", alloy::hex::encode(sv::factory::terms_hash(terms))),
    })
}

/// Whether `signer` is blessed for `vault` and the bundle it runs, and as an
/// executor, at `now`: the registry's blessing for the vault, the key's
/// record, and its image's and project's, as the accounts read have them,
/// judged as the programs judge them.
fn blessed_executor(
    served: &SolanaServed,
    accounts: &Accounts,
    vault: &Pubkey,
    signer: &Pubkey,
    now: u64,
) -> bool {
    use chain::solana::programs::registry::{read as at, state};

    let Some(account) = decode::<sv::state::Vault>(accounts, vault) else {
        return false;
    };
    let Some(config) = decode::<state::Config>(accounts, &at::config_address()) else {
        return false;
    };
    let Some(blessing) = decode::<state::Blessing>(
        accounts,
        &at::blessing_address(
            signer,
            &served.platform.cluster,
            &vault.to_bytes(),
            &account.bundle_hash,
        ),
    ) else {
        return false;
    };
    let Some(key) = decode::<state::KeyInfo>(accounts, &at::key_address(signer)) else {
        return false;
    };
    let image = decode::<state::ImageRecord>(accounts, &at::image_address(&key.image_digest));
    let project = decode::<state::ProjectRecord>(accounts, &at::project_address(&key.project_id));
    blessing.signer == *signer
        && state::is_blessed_for(&config, &blessing, image.as_ref(), project.as_ref(), now)
        && state::is_blessed_as(
            &config,
            &key,
            image.as_ref(),
            project.as_ref(),
            state::ImageKind::Executor,
            now,
        )
}

/// Every vault the vault program holds, by strategy id.
pub async fn vaults(client: &SolanaClient) -> Result<Vec<(Pubkey, sv::state::Vault)>> {
    let accounts = client
        .rpc
        .program_accounts(&sv::ID, &[(0, sv::state::Vault::DISCRIMINATOR.to_vec())])
        .await
        .context("listing the vaults")?;
    let mut vaults = Vec::new();
    for (address, account) in accounts {
        let vault = sv::state::Vault::try_deserialize(&mut &account.data[..])
            .map_err(|error| anyhow!("the vault at {address} does not read as one: {error}"))?;
        vaults.push((address, vault));
    }
    vaults.sort_by_key(|(_, vault)| vault.strategy_id);
    Ok(vaults)
}

fn author_of(key: &Pubkey, profile: Option<profiles::Profile>) -> Author {
    let profile = profile.filter(|profile| !profile.handle.is_empty());
    Author {
        handle: profile
            .as_ref()
            .map_or_else(|| key.to_string(), |profile| profile.handle.clone()),
        joined: day(profile.as_ref().map_or(0, |profile| profile.joined_at)),
        blurb: profile
            .as_ref()
            .map(|profile| profile.blurb.clone())
            .filter(|blurb| !blurb.is_empty())
            .unwrap_or_else(|| "This author has not said anything about themselves.".to_string()),
        links: profile
            .map(|profile| {
                profile
                    .link_labels
                    .into_iter()
                    .zip(profile.link_urls)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// A queued build with the review and activation its cluster recorded.
/// The queue's completion timeout and retry cache bound these reads exactly
/// as they do an EVM job's reads.
pub(crate) async fn job(served: &SolanaServed, store: &Store, mut job: Job) -> Result<Job> {
    if let Ok(author) = job.author.parse::<Pubkey>()
        && let Some(profile) = served.platform.profile(&served.client, &author).await?
        && !profile.handle.is_empty()
    {
        job.author = profile.handle;
    }
    let Some(id) = job.strategy_id else {
        return Ok(job);
    };
    job.review = served
        .platform
        .review(&served.client, id)
        .await?
        .filter(|review| review.reviewed_at != 0)
        .map(|review| review_of(store, &review));
    let accounts = served.platform.vault_accounts(id);
    if served
        .client
        .load::<sv::state::Vault>(&accounts.vault)
        .await?
        .is_some()
    {
        job.state = JobState::Activated;
        job.strategy_slug = Some(Strategy::slug_for(job.chain, id));
    }
    Ok(job)
}

/// The accounts of every claim the vault's history says it booked.
fn claim_addresses(history: &History, vault: &Pubkey) -> Vec<Pubkey> {
    history
        .vaults
        .get(vault)
        .into_iter()
        .flat_map(|log| log.claims.keys())
        .map(|id| sv::state::claim_address(vault, *id).0)
        .collect()
}

/// What a vault's accounts (the vault, its claims as far as its history
/// names them, its list of restricted places) come to, to tell whether they
/// changed.
fn digest(accounts: &Accounts, address: &Pubkey, claims: &[Pubkey]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data(accounts, address).hash(&mut hasher);
    for at in claims {
        at.hash(&mut hasher);
        data(accounts, at).hash(&mut hasher);
    }
    data(accounts, &sv::state::restricted_address(address).0).hash(&mut hasher);
    hasher.finish()
}

#[allow(clippy::too_many_arguments)]
async fn one(
    served: &SolanaServed,
    store: &Store,
    history: &mut History,
    health: &HashMap<Pubkey, Health>,
    accounts: &Accounts,
    address: Pubkey,
    vault: &sv::state::Vault,
    now: u64,
    transactions_per_refresh: usize,
) -> Result<Strategy> {
    let client = &served.client;
    let id = vault.strategy_id;
    let registered =
        decode::<publishing::state::Strategy>(accounts, &publishing::state::strategy_address(id));
    let review = decode::<publishing::state::Review>(accounts, &review_address(id));
    let author = match decode::<profiles::Profile>(accounts, &profile_address(&vault.author)) {
        Some(profile) if !profile.handle.is_empty() => profile.handle,
        _ => vault.author.to_string(),
    };

    // Its requests and history, read again only once its accounts changed,
    // or while its history is still being read.
    let before = digest(accounts, &address, &claim_addresses(history, &address));
    let log = history.vaults.entry(address).or_default();
    let changed = history.digests.get(&address) != Some(&before);
    // The accounts of claims the history names for the first time, read now
    // rather than a refresh later.
    let mut claim_accounts = Accounts::new();
    if changed || !log.queue.is_empty() || !history.requests.contains_key(&address) {
        read_history(client, &address, log, transactions_per_refresh).await?;
        let waiting = requests(client, &address).await?;
        history.requests.insert(address, waiting);
        let named = claim_addresses(history, &address);
        let unread: Vec<Pubkey> = named
            .iter()
            .filter(|at| !accounts.contains_key(at))
            .copied()
            .collect();
        if !unread.is_empty() {
            let (_, read) = client
                .rpc
                .accounts_since(&unread, history.slot)
                .await
                .context("reading the vault's claims")?;
            claim_accounts.extend(unread.into_iter().zip(read));
        }
        let mut seen = accounts.clone();
        seen.extend(claim_accounts.clone());
        history
            .digests
            .insert(address, digest(&seen, &address, &named));
    }
    let log = history.vaults.entry(address).or_default();
    let requests = history.requests.get(&address).cloned().unwrap_or_default();

    let claims = claims(accounts, &claim_accounts, &address, vault, &log.claims)?;
    let restricted: Vec<String> =
        decode::<sv::state::Restricted>(accounts, &sv::state::restricted_address(&address).0)
            .map(|list| {
                list.codes
                    .iter()
                    .map(|code| name_of(B256::from(*code)))
                    .collect()
            })
            .unwrap_or_default();

    let figures = Figures::of(vault);
    let days_live = now.saturating_sub(vault.created_at) / 86_400;
    let shown = shown_vault(
        served.vault_chain,
        &address,
        vault,
        &figures,
        claims,
        requests,
        restricted,
    );
    let record = read::record_from(&log.samples, &shown, now, 0);

    let kill = log.killed.as_ref().map(|event| Kill {
        at: event.at,
        reason: "the admin key stopped it".to_string(),
        outcome: if vault.state == ChainVaultState::Closed {
            "every position was closed and the cash brought home; holders are paid from it"
                .to_string()
        } else {
            "its positions are being closed and the cash brought home".to_string()
        },
        reference: event.reference.clone(),
    });
    let standing = match standing(vault.state, days_live) {
        Standing::Closed if kill.is_some() => Standing::Killed,
        other => other,
    };

    let manifest = registered
        .as_ref()
        .and_then(|registered| store.manifest(&hash_of(B256::from(registered.manifest_hash))))
        .map(|canonical| canonical.manifest);
    let bundle = read::bundle_from(
        store,
        B256::from(registered.as_ref().map_or([0; 32], |r| r.manifest_hash)),
        B256::from(registered.as_ref().map_or([0; 32], |r| r.source_hash)),
        B256::from(vault.bundle_hash),
    );
    let station = data(accounts, &vault.station)
        .and_then(chain::solana::programs::station::state::Station::read_any)
        .map(|(station, _)| station);
    let legs = vec![leg(&shown, vault, station.as_ref(), &figures)];
    let events: Vec<VaultEvent> = log.events.iter().rev().cloned().collect();

    Ok(Strategy {
        id,
        slug: Strategy::slug_for(served.vault_chain, id),
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
        author,
        author_address: vault.author.to_string(),
        standing,
        registered: day(registered
            .as_ref()
            .map_or(vault.created_at, |r| r.registered_at)),
        days_live: days_live as u32,
        bundle,
        vault: shown,
        legs,
        review: review
            .filter(|review| review.is_review())
            .map(|review| review_of(store, &review)),
        health: health.get(&address).cloned(),
        attestation: None,
        record,
        trades: Vec::new(),
        events,
        kill,
        restricted: registered
            .as_ref()
            .map(|r| {
                r.lists
                    .restricted
                    .iter()
                    .map(|code| name_of(B256::from(*code)))
                    .collect()
            })
            .unwrap_or_default(),
        drivers: vault
            .drivers
            .iter()
            .map(|driver| name_of(B256::from(*driver)))
            .collect(),
        bridges: registered
            .as_ref()
            .map(|r| {
                r.lists
                    .bridges
                    .iter()
                    .map(|b| name_of(B256::from(*b)))
                    .collect()
            })
            .unwrap_or_default(),
        stale_as_of: None,
    })
}

/// A vault's worth and price per share, from its last priced valuation: the
/// price every request it settles is priced at. Its worth is every share
/// counted now at that price, so a deposit or withdrawal settled since the
/// valuation (at exactly that price) is in it, as the shares it made or
/// burnt. A vault never priced is at a dollar a share; a closed one is worth
/// its cash.
struct Figures {
    value: f64,
    share_price: f64,
    /// What the station held at the valuation, at mark.
    at_station: f64,
}

impl Figures {
    fn of(vault: &sv::state::Vault) -> Self {
        let latest = &vault.latest;
        let shares = vault.total_supply as f64 / SHARE;
        if vault.state == ChainVaultState::Closed {
            let supply = vault.total_supply.saturating_add(sv::state::OFFSET) as f64;
            let cash = vault.cash as f64;
            return Self {
                value: cash / DOLLAR,
                share_price: ((cash + 1.0) / DOLLAR) / (supply / SHARE),
                at_station: 0.0,
            };
        }
        if !vault.priced || latest.supply == 0 {
            return Self {
                value: shares,
                share_price: 1.0,
                at_station: 0.0,
            };
        }
        let (share_price, _) = valued(latest.gross, latest.fee, latest.supply);
        Self {
            value: shares * share_price,
            share_price,
            at_station: latest
                .gross
                .saturating_sub(1)
                .saturating_sub(latest.cash as u128) as f64
                / DOLLAR,
        }
    }
}

/// A valuation's price per share after the fee it owes, and what the vault
/// was worth at mark: its gross carries one virtual unit, its supply the
/// virtual shares, so a vault with nothing in it is at a dollar a share.
fn valued(gross: u128, fee: u128, supply: u128) -> (f64, f64) {
    let net = gross.saturating_sub(fee) as f64 / DOLLAR;
    let shares = supply as f64 / SHARE;
    let per_share = if shares > 0.0 { net / shares } else { 1.0 };
    (per_share, gross.saturating_sub(1) as f64 / DOLLAR)
}

fn shown_vault(
    vault_chain: Chain,
    address: &Pubkey,
    vault: &sv::state::Vault,
    figures: &Figures,
    claims: Vec<Claim>,
    requests: Vec<Request>,
    restricted: Vec<String>,
) -> Vault {
    let allowances = shared::Allowances {
        fuel_per_sec: vault.allowances.fuel_per_sec,
        memory_mib: vault.allowances.memory_mib,
        requests_per_min: vault.allowances.requests_per_min,
        bytes_per_day: vault.allowances.bytes_per_day,
        streams: vault.allowances.streams,
        inference_ms_per_min: vault.allowances.inference_ms_per_min,
        gas_per_day: vault.allowances.gas_per_day as f64 / 1_000_000.0,
    };
    let daily = shared::Prices::published().daily_cap(&allowances);
    Vault {
        address: address.to_string(),
        chain: vault_chain,
        asset: vault_chain.dollar().to_string(),
        state: match vault.state {
            ChainVaultState::None => VaultState::Activating,
            ChainVaultState::Open => VaultState::Open,
            ChainVaultState::WindingDown => VaultState::WindingDown,
            ChainVaultState::Frozen => VaultState::Frozen,
            ChainVaultState::Killed => VaultState::Killed,
            ChainVaultState::Closed => VaultState::Closed,
        },
        created: day(vault.created_at),
        station: vault.station.to_string(),
        value: figures.value,
        valued_at: vault.priced.then(|| moment(vault.latest.time)),
        idle: vault.cash as f64 / DOLLAR,
        escrow: vault.escrow as f64 / DOLLAR,
        shares: vault.total_supply as f64 / SHARE,
        share_price: figures.share_price,
        // `1e18` is $1.00 a share.
        high_water_mark: vault.mark as f64 / SHARE,
        last_crystallised: day(vault.last_payment),
        idle_floor_bps: vault.floor_bps as u32,
        pending_floor: (vault.pending_floor_at != 0).then(|| PendingFloor {
            bps: vault.pending_floor_bps as u32,
            from: moment(vault.pending_floor_at),
        }),
        allowances,
        requests,
        restricted,
        halt: (vault.halted_at != 0).then(|| Halt {
            at: moment(vault.halted_at),
            reason: halt_reason(vault.halt_reason),
        }),
        fees: Fees {
            holding_bps: vault.fees.holding_bps as u32,
            author_bps: vault.fees.author_bps as u32,
            platform_bps: vault.fees.platform_bps as u32,
            transaction_bps: vault.fees.transaction_bps as u32,
        },
        exit_band_bps: vault.exit_band_bps as u32,
        tank: Tank {
            balance: vault.tank.balance as f64 / DOLLAR,
            daily_charge: daily,
            keep_at: vault.tank.keep_at as f64 / DOLLAR,
            usage_settled: vault.tank.usage_settled as f64 / DOLLAR,
        },
        // A Solana vault trades on its own chain only: its station serves
        // every claim itself.
        exit_class: ExitClass::Direct,
        freeze_after_days: (vault.timers.freeze_after / 86_400) as u32,
        claims,
        in_transit: None,
    }
}

fn halt_reason(reason: HaltReason) -> String {
    match reason {
        HaltReason::Killed => "the admin key stopped it",
        HaltReason::Frozen => "a claim went unpaid for its whole freeze period",
        HaltReason::Retired => "its author retired it and the wind-down ended",
        HaltReason::Sealed => "it is being wound down",
        HaltReason::None => "",
    }
    .to_string()
}

fn standing(state: ChainVaultState, days_live: u64) -> Standing {
    match state {
        ChainVaultState::Killed => Standing::Killed,
        ChainVaultState::Frozen => Standing::Frozen,
        ChainVaultState::Closed => Standing::Closed,
        ChainVaultState::WindingDown => Standing::Retiring,
        _ if days_live >= Strategy::DAYS_NEW as u64 => Standing::Listed,
        _ => Standing::New,
    }
}

/// What a depositor is shown a driver as: its venue, as the published list
/// of drivers labels it, or its name where the list has none.
pub fn venue_of(driver: &str) -> String {
    shared::catalogue::Catalogue::published()
        .drivers
        .get(driver)
        .map(|entry| entry.label.clone())
        .unwrap_or_else(|| driver.to_string())
}

/// The venues a station trades at: each driver it pinned, by the label the
/// published list gives it, in the order pinned; the vault's own drivers'
/// where the station could not be read.
fn venues(vault: &sv::state::Vault, station: Option<&station_state::Station>) -> String {
    let names: Vec<String> = match station {
        Some(station) if !station.drivers.is_empty() => station
            .drivers
            .iter()
            .map(|pinned| name_of(B256::from(pinned.name)))
            .collect(),
        _ => vault
            .drivers
            .iter()
            .map(|driver| name_of(B256::from(*driver)))
            .collect(),
    };
    let mut labels: Vec<String> = Vec::new();
    for name in names.iter().filter(|name| !name.is_empty()) {
        let label = venue_of(name);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    match labels.as_slice() {
        [] => "its station's venue".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The vault's one leg: its station, at the venues of the drivers it
/// pinned.
fn leg(
    shown: &Vault,
    vault: &sv::state::Vault,
    station: Option<&station_state::Station>,
    figures: &Figures,
) -> Leg {
    Leg {
        venue: venues(vault, station),
        chain: shown.chain,
        custody: Custody::VenueEnforced,
        exit_path: ExitPath::Contract,
        last_resort: None,
        share_of_value: if figures.value > 0.0 {
            (figures.at_station / figures.value).clamp(0.0, 1.0)
        } else {
            0.0
        },
        valuation: Valuation::OnChain {
            source: "the pinned driver's reads of the venue's accounts".to_string(),
        },
        account: vault.station.to_string(),
        key: "the executor's blessed key, through the station's guard".to_string(),
        can: vec!["send the venue instructions the station's allowlist names".to_string()],
        cannot: vec![
            "withdraw anything".to_string(),
            "call a program the station did not pin".to_string(),
            "send a dollar anywhere but the vault's own accounts".to_string(),
        ],
        status: VenueStatus::Ok,
        note: None,
    }
}

fn review_of(store: &Store, review: &publishing::state::Review) -> Review {
    let report = store.report(&hash_of(B256::from(review.report_hash)));
    let missing = || "The report is not in the store.".to_string();
    Review {
        ratings: review
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
        reviewed: day(review.reviewed_at),
        refused: review.refused,
        sealed: report.as_ref().is_some_and(|report| report.sealed),
        reviewer_image: hash_of(B256::from(review.reviewer_image)),
        model_id_hash: hash_of(B256::from(review.model_id_hash)),
        prompt_hash: hash_of(B256::from(review.prompt_hash)),
        report_hash: hash_of(B256::from(review.report_hash)),
        limits: report
            .map(|report| report.limits)
            .unwrap_or_else(|| vec![missing()]),
    }
}

/// Every claim the vault's history says it booked, each at the address its
/// id derives: as its account has it while the account is there (`accounts`,
/// or `more` for one only just named), and as the history tells of it once
/// the account is closed.
fn claims(
    accounts: &Accounts,
    more: &Accounts,
    address: &Pubkey,
    vault: &sv::state::Vault,
    booked: &BTreeMap<u64, Booked>,
) -> Result<Vec<Claim>> {
    let mut out = Vec::new();
    for (&id, told) in booked {
        let at = sv::state::claim_address(address, id).0;
        let read = if accounts.contains_key(&at) {
            accounts
        } else if more.contains_key(&at) {
            more
        } else {
            anyhow::bail!("claim {id} was not read");
        };
        let (owner, shares, booked_at, state, paid) = match data(read, &at) {
            Some(bytes) => {
                let claim = sv::state::Claim::try_deserialize(&mut &bytes[..])
                    .map_err(|error| anyhow!("claim {id} does not read as one: {error}"))?;
                // A claim paid in parts counts down its shares as it goes:
                // what it is shown at is what it was booked for.
                (
                    claim.owner,
                    told.shares.max(claim.shares),
                    claim.booked_at,
                    claim.state,
                    claim.paid,
                )
            }
            // Closed once it was paid (or taken through the fallback) and
            // its rent taken back: what it was is in the vault's history.
            None => (
                told.owner,
                told.shares,
                told.at,
                if told.paid > 0 {
                    ChainClaimState::Paid
                } else {
                    ChainClaimState::Fallback
                },
                told.paid,
            ),
        };
        let burnt = matches!(state, ChainClaimState::Paid | ChainClaimState::Fallback);
        // A paid claim's shares are burnt, so the supply no longer counts
        // them: its share is measured with them put back.
        let supply = if burnt {
            vault.total_supply.saturating_add(shares)
        } else {
            vault.total_supply
        };
        out.push(Claim {
            id: id as u32,
            owner: owner.to_string(),
            shares: shares as f64 / SHARE,
            fraction_closed: if supply == 0 {
                0.0
            } else {
                shares as f64 / supply as f64
            },
            booked: day(booked_at),
            state: match state {
                ChainClaimState::Paid => ClaimState::Paid,
                ChainClaimState::Waiting => ClaimState::Waiting,
                ChainClaimState::Queued => ClaimState::Queued,
                ChainClaimState::Fallback => ClaimState::Fallback,
                ChainClaimState::Pending | ChainClaimState::None => ClaimState::Pending,
            },
            settled_for: (paid > 0).then(|| paid as f64 / DOLLAR),
        });
    }
    Ok(out)
}

/// Every request still waiting to be priced, oldest first: the vault's
/// request accounts not yet done. A request is at its maker's address, so
/// they are found by the vault they name.
async fn requests(client: &SolanaClient, address: &Pubkey) -> Result<Vec<Request>> {
    let accounts = client
        .rpc
        .program_accounts(
            &sv::ID,
            &[
                (0, sv::state::Request::DISCRIMINATOR.to_vec()),
                (8, address.to_bytes().to_vec()),
            ],
        )
        .await
        .context("listing the vault's requests")?;
    let mut waiting = Vec::new();
    for (at, account) in accounts {
        let request = sv::state::Request::try_deserialize(&mut &account.data[..])
            .map_err(|error| anyhow!("the request at {at} does not read as one: {error}"))?;
        if request.done || request.vault != *address {
            continue;
        }
        let kind = match request.kind {
            ChainRequestKind::Deposit => RequestKind::Deposit,
            ChainRequestKind::Redeem => RequestKind::Withdrawal,
            ChainRequestKind::Claim => RequestKind::Claim,
            ChainRequestKind::TankFill => RequestKind::TankFill,
        };
        let in_dollars = kind == RequestKind::Deposit
            || (kind == RequestKind::TankFill && request.reference == 1);
        waiting.push(Request {
            id: request.id,
            owner: request.owner.to_string(),
            kind,
            amount: request.amount as f64 / if in_dollars { DOLLAR } else { SHARE },
            earliest: moment(request.earliest),
        });
    }
    waiting.sort_by_key(|request| request.id);
    Ok(waiting)
}

/// Read whatever is new in the vault's history: list the transactions that
/// touched it since the last listing, then read them oldest first, at most
/// `transactions_per_refresh` a refresh. A transaction that can't be read
/// is read again next time, so nothing is passed over; one the node no
/// longer holds is passed over, as there is nothing else to read it from.
async fn read_history(
    client: &SolanaClient,
    vault: &Pubkey,
    log: &mut VaultLog,
    transactions_per_refresh: usize,
) -> Result<()> {
    let mut newest_first = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let page = client
            .rpc
            .signatures_for(
                vault,
                SIGNATURE_PAGE,
                before.as_deref(),
                log.listed_until.as_deref(),
            )
            .await
            .context("listing the vault's transactions")?;
        let full = page.len() == SIGNATURE_PAGE;
        before = page.last().map(|info| info.signature.clone());
        newest_first.extend(page);
        if !full {
            break;
        }
    }
    if let Some(newest) = newest_first.first() {
        log.listed_until = Some(newest.signature.clone());
    }
    for info in newest_first.into_iter().rev() {
        if info.err.is_none() {
            log.queue.push_back(info.signature);
        }
    }

    let mut read = 0;
    while read < transactions_per_refresh.max(1) {
        let Some(signature) = log.queue.front().cloned() else {
            break;
        };
        read += 1;
        let record = client
            .transaction(&signature)
            .await
            .with_context(|| format!("reading the vault's transaction {signature}"))?;
        if let Some(record) = record {
            fold(log, vault, &record);
        } else {
            tracing::warn!(%vault, %signature, "a transaction the node no longer holds was passed over");
        }
        log.queue.pop_front();
    }
    log.read_through = log.queue.is_empty();
    if !log.read_through {
        tracing::debug!(%vault, left = log.queue.len(), "read part of a vault's history; the rest next time");
    }
    Ok(())
}

/// What the vault program said in a transaction about `vault`: the events it
/// emitted through its own instruction data under an instruction naming the
/// vault, then those it wrote to the logs while it was the program running.
pub fn said_about(record: &TransactionRecord, vault: &Pubkey) -> Vec<Vec<u8>> {
    let ours = |index: usize| {
        record
            .instructions
            .get(index)
            .is_some_and(|ix| ix.accounts.contains(vault))
    };
    let mut out = Vec::new();
    for (index, calls) in &record.inner {
        if !ours(*index) {
            continue;
        }
        for call in calls {
            if call.program != sv::ID {
                continue;
            }
            if let Some(event) = call
                .data
                .strip_prefix(chain::solana::programs::anchor_lang::event::EVENT_IX_TAG_LE)
            {
                out.push(event.to_vec());
            }
        }
    }
    out.extend(logged_by(&record.logs, &sv::ID));
    out
}

/// The data `program` wrote to the logs (`Program data:`) while it was the
/// one running, so another program's event of the same name is never taken
/// for one of its own.
pub fn logged_by(logs: &[String], program: &Pubkey) -> Vec<Vec<u8>> {
    let program = program.to_string();
    let mut running: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for line in logs {
        if let Some(rest) = line.strip_prefix("Program ") {
            if let Some(data) = rest.strip_prefix("data: ") {
                if running.last() == Some(&program.as_str())
                    && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim())
                {
                    out.push(bytes);
                }
                continue;
            }
            let mut words = rest.split_whitespace();
            let (Some(id), Some(what)) = (words.next(), words.next()) else {
                continue;
            };
            match what {
                "invoke" => running.push(id),
                "success" | "failed:" | "failed" => {
                    running.pop();
                }
                _ => {}
            }
        }
    }
    out
}

/// One event of kind `E`, from its discriminator and its body.
fn decoded<E: Discriminator + AnchorDeserialize>(data: &[u8]) -> Option<E> {
    let mut body = data.strip_prefix(E::DISCRIMINATOR)?;
    E::deserialize(&mut body).ok()
}

/// Fold one transaction into what is known of the vault.
fn fold(log: &mut VaultLog, vault: &Pubkey, record: &TransactionRecord) {
    let at = record.block_time.unwrap_or(0).max(0) as u64;
    let reference = record.signature.clone();
    // The withdrawal said just before, if the event before was one: a claim
    // paid while the vault ran follows its own withdrawal, which already
    // took it off the owner's cost; one the closed vault paid says nothing
    // before it, and is taken off the cost of whoever booked it.
    let mut withdrawn_just_before: Option<u64> = None;
    for data in said_about(record, vault) {
        let withdrawn_before = withdrawn_just_before.take();
        if let Some(valued) = decoded::<ev::Valued>(&data) {
            if log.last_valuation.is_none_or(|last| valued.index > last) {
                log.last_valuation = Some(valued.index);
                let (value_per_share, aum) = valued_sample(&valued);
                keep_sample(
                    &mut log.samples,
                    Sample {
                        at: valued.time,
                        value_per_share,
                        aum,
                    },
                );
            }
            continue;
        }
        if let Some(deposited) = decoded::<ev::Deposited>(&data) {
            *log.paid.entry(deposited.owner).or_default() += deposited.assets as f64 / DOLLAR;
        } else if let Some(withdrawn) = decoded::<ev::Withdrawn>(&data) {
            *log.paid.entry(withdrawn.owner).or_default() -= withdrawn.assets as f64 / DOLLAR;
            withdrawn_just_before = Some(withdrawn.assets);
        } else if let Some(taken) = decoded::<ev::ProRataTaken>(&data) {
            *log.paid.entry(taken.owner).or_default() -= taken.assets as f64 / DOLLAR;
        } else if let Some(paid) = decoded::<ev::UnitsPaid>(&data) {
            *log.paid.entry(paid.owner).or_default() -= paid.assets as f64 / DOLLAR;
        } else if let Some(booked) = decoded::<ev::ClaimBooked>(&data) {
            log.claims.entry(booked.claim_id).or_insert(Booked {
                owner: booked.owner,
                shares: booked.shares,
                at,
                paid: 0,
            });
        } else if let Some(paid) = decoded::<ev::ClaimPaid>(&data)
            && let Some(claim) = log.claims.get_mut(&paid.claim_id)
        {
            claim.paid = claim.paid.saturating_add(paid.assets);
            if withdrawn_before != Some(paid.assets) {
                *log.paid.entry(claim.owner).or_default() -= paid.assets as f64 / DOLLAR;
            }
        }
        if let Some((kind, summary, amount)) = event_of(&data) {
            let event = VaultEvent {
                at: moment(at),
                kind,
                summary,
                amount,
                reference: reference.clone(),
            };
            if kind == EventKind::Killed {
                log.killed = Some(event.clone());
            }
            log.events.push_back(event);
            if log.events.len() > SHOWN {
                log.events.pop_front();
            }
        }
    }
}

fn valued_sample(event: &ev::Valued) -> (f64, f64) {
    valued(event.gross, event.fee, event.supply)
}

/// Keep one reading an hour, the last of each, and prune as the EVM record
/// does: a vault valued every few seconds of chain time keeps no more than
/// the record shows.
fn keep_sample(samples: &mut Vec<Sample>, sample: Sample) {
    match samples.last_mut() {
        Some(last) if last.at / HOUR == sample.at / HOUR => *last = sample,
        Some(last) if last.at > sample.at => {}
        _ => samples.push(sample),
    }
    read::prune(samples, sample.at);
}

/// A vault event as the page tells of it, if it tells of it at all.
fn event_of(data: &[u8]) -> Option<(EventKind, String, Option<f64>)> {
    let dollars = |units: u64| Some(units as f64 / DOLLAR);
    Some(if let Some(e) = decoded::<ev::Requested>(data) {
        match e.kind {
            ChainRequestKind::Deposit => (
                EventKind::Requested,
                "a deposit was asked for, to be priced at a later valuation".to_string(),
                Some(e.amount as f64 / DOLLAR),
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
            ChainRequestKind::TankFill => (
                EventKind::Requested,
                "the tank asked to be topped up from the author's shares".to_string(),
                None,
            ),
        }
    } else if let Some(e) = decoded::<ev::ClaimTried>(data) {
        (
            EventKind::ClaimTried,
            if e.blocked {
                format!(
                    "claim {} was tried; a market outside the exit band held its close",
                    e.claim_id
                )
            } else {
                format!("claim {} was tried", e.claim_id)
            },
            None,
        )
    } else if let Some(e) = decoded::<ev::ProRataTaken>(data) {
        (
            EventKind::FallbackTaken,
            "a holder nothing could price took the pro-rata fallback".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::UnitsPaid>(data) {
        (
            EventKind::FallbackTaken,
            "a fallback holder was paid for their units".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::StateChanged>(data) {
        match e.state {
            ChainVaultState::Closed => (
                EventKind::Closed,
                "the vault closed; its holders are paid from its cash".to_string(),
                None,
            ),
            _ => return None,
        }
    } else if let Some(e) = decoded::<ev::Deposited>(data) {
        (
            EventKind::Deposited,
            "a deposit was priced".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::Withdrawn>(data) {
        (
            EventKind::Withdrawn,
            "a withdrawal was priced and paid".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::Refunded>(data) {
        (
            EventKind::Refunded,
            "a deposit the vault could no longer take went back".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::ClaimBooked>(data) {
        (
            EventKind::ClaimBooked,
            format!(
                "a withdrawal the cash could not cover: claim {}",
                e.claim_id
            ),
            None,
        )
    } else if let Some(e) = decoded::<ev::ClaimPaid>(data) {
        (
            EventKind::ClaimSettled,
            format!("claim {} paid", e.claim_id),
            dollars(e.assets),
        )
    } else if decoded::<ev::FeesPaid>(data).is_some() {
        (
            EventKind::Crystallised,
            "the week's fees were paid as new shares".to_string(),
            None,
        )
    } else if let Some(e) = decoded::<ev::ToStation>(data) {
        (
            EventKind::CapitalMoved,
            "cash to the station".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::FromStation>(data) {
        (
            EventKind::CapitalMoved,
            "the station to cash".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::TankFunded>(data) {
        (
            EventKind::TankFunded,
            "the tank was topped up".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::TankFilled>(data) {
        (
            EventKind::TankFunded,
            "the tank was topped up from the author's shares".to_string(),
            dollars(e.assets),
        )
    } else if let Some(e) = decoded::<ev::TankCharged>(data) {
        (
            EventKind::TankCharged,
            "the tank paid for running the agent".to_string(),
            dollars(e.charged),
        )
    } else {
        let e = decoded::<ev::Halted>(data)?;
        match e.reason {
            HaltReason::Frozen => (
                EventKind::Frozen,
                "a claim went untried and the vault froze".to_string(),
                None,
            ),
            HaltReason::Retired => (
                EventKind::Retired,
                "the author's wind-down ended".to_string(),
                None,
            ),
            HaltReason::Sealed => (
                EventKind::Killed,
                "the vault is being wound down".to_string(),
                None,
            ),
            _ => (EventKind::Killed, "the vault was stopped".to_string(), None),
        }
    })
}

/// What `owner` has paid for what they still hold in `vault`, as its history
/// has been read so far: everything they put in, less everything they took
/// out. None while the vault's history has not been read.
pub fn cost(history: &History, vault: &Pubkey, owner: &Pubkey) -> Option<f64> {
    let log = history.vaults.get(vault)?;
    Some(log.paid.get(owner).copied().unwrap_or(0.0).max(0.0))
}

/// What is worth announcing on a Solana chain, newest first.
pub fn feed(vault_chain: Chain, strategies: &[Strategy]) -> Vec<FeedEvent> {
    let mut out = Vec::new();
    for strategy in strategies {
        if read::never_read(strategy) {
            continue;
        }
        out.push(FeedEvent {
            at: strategy.registered.midnight(),
            kind: FeedKind::StrategyRegistered,
            title: format!("Agent {} on {} was registered", strategy.id, vault_chain.label()),
            detail: "The reviewer read it first, then an allowlisted build service compiled it and signed what it made."
                .to_string(),
            link: Some(format!("/agents/{}", strategy.slug)),
            reference: strategy.bundle.bundle_hash.clone(),
        });
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
                at: review.reviewed.midnight(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::Called;
    use chain::solana::programs::anchor_lang::Event;

    fn a_record(
        instructions: Vec<Called>,
        inner: Vec<(usize, Vec<Called>)>,
        logs: Vec<String>,
    ) -> TransactionRecord {
        TransactionRecord {
            signature: "sig".to_string(),
            slot: 1,
            block_time: Some(1_800_000_000),
            err: None,
            fee: 5_000,
            account_keys: Vec::new(),
            pre_balances: Vec::new(),
            post_balances: Vec::new(),
            instructions,
            inner,
            logs,
            units: None,
        }
    }

    fn a_valuation(index: u64, gross: u128) -> ev::Valued {
        ev::Valued {
            index,
            time: 1_800_000_000 + index * HOUR,
            gross,
            gross_exit: gross,
            supply: sv::state::OFFSET + 100 * 1_000_000_000_000_000_000,
            fee: 0,
        }
    }

    fn emitted(event: &impl Event) -> Called {
        Called {
            program: sv::ID,
            accounts: vec![],
            data: [
                chain::solana::programs::anchor_lang::event::EVENT_IX_TAG_LE,
                &event.data(),
            ]
            .concat(),
        }
    }

    /// A transaction that values two vaults, one instruction each: each
    /// vault's record takes only the valuation made under the instruction
    /// that named it.
    #[test]
    fn a_valuation_is_taken_only_under_the_instruction_that_named_its_vault() {
        let ours = Pubkey::new_unique();
        let theirs = Pubkey::new_unique();
        let call = |vault: Pubkey| Called {
            program: sv::ID,
            accounts: vec![Pubkey::new_unique(), vault],
            data: vec![],
        };
        let record = a_record(
            vec![call(theirs), call(ours)],
            vec![
                (0, vec![emitted(&a_valuation(1, 500_000_001))]),
                (1, vec![emitted(&a_valuation(1, 120_000_001))]),
            ],
            vec![],
        );
        let mut log = VaultLog::default();
        fold(&mut log, &ours, &record);
        assert_eq!(log.samples.len(), 1);
        assert!(
            (log.samples[0].aum - 120.0).abs() < 1e-9,
            "{}",
            log.samples[0].aum
        );
        assert!((log.samples[0].value_per_share - 1.2).abs() < 1e-6);

        // The same valuation read twice is kept once.
        fold(&mut log, &ours, &record);
        assert_eq!(log.samples.len(), 1);
    }

    /// The venue has events named as the vault's are; only what the vault
    /// program itself logged is read as the vault's.
    #[test]
    fn only_what_the_vault_program_logged_is_read_as_its_event() {
        let vault = Pubkey::new_unique();
        let venue = Pubkey::new_unique();
        let deposited = ev::Deposited {
            request_id: 1,
            owner: Pubkey::new_unique(),
            assets: 25_000_000,
            shares: 25,
        };
        let line = format!(
            "Program data: {}",
            base64::engine::general_purpose::STANDARD.encode(deposited.data())
        );
        let logs = vec![
            format!("Program {} invoke [1]", sv::ID),
            format!("Program {venue} invoke [2]"),
            line.clone(),
            format!("Program {venue} success"),
            line,
            format!("Program {} success", sv::ID),
        ];
        let mut log = VaultLog::default();
        fold(&mut log, &vault, &a_record(vec![], vec![], logs));
        assert_eq!(log.paid.get(&deposited.owner).copied(), Some(25.0));
        assert_eq!(log.events.len(), 1);
        assert_eq!(log.events[0].kind, EventKind::Deposited);
    }

    /// A claim the closed vault pays says only its id: it comes off the cost
    /// of whoever booked it. One paid while the vault ran follows its own
    /// withdrawal, and comes off once.
    #[test]
    fn a_claim_the_closed_vault_pays_comes_off_its_owner_s_cost_once() {
        let vault = Pubkey::new_unique();
        let (owner, other) = (Pubkey::new_unique(), Pubkey::new_unique());
        let under_vault = |events: Vec<Called>| {
            let named = Called {
                program: sv::ID,
                accounts: vec![vault],
                data: vec![],
            };
            a_record(vec![named], vec![(0, events)], vec![])
        };
        let mut log = VaultLog::default();
        for (who, assets) in [(owner, 100_000_000), (other, 50_000_000)] {
            fold(
                &mut log,
                &vault,
                &under_vault(vec![emitted(&ev::Deposited {
                    request_id: 1,
                    owner: who,
                    assets,
                    shares: 1,
                })]),
            );
        }
        for (claim_id, who) in [(7, owner), (8, other)] {
            fold(
                &mut log,
                &vault,
                &under_vault(vec![emitted(&ev::ClaimBooked {
                    claim_id,
                    owner: who,
                    shares: 1,
                })]),
            );
        }
        // Claim 8, paid while the vault ran: its withdrawal, then the claim.
        fold(
            &mut log,
            &vault,
            &under_vault(vec![
                emitted(&ev::Withdrawn {
                    request_id: 0,
                    owner: other,
                    shares: 1,
                    assets: 20_000_000,
                    fee_shares: 0,
                }),
                emitted(&ev::ClaimPaid {
                    claim_id: 8,
                    assets: 20_000_000,
                }),
            ]),
        );
        // Claim 7, paid by the closed vault: the claim alone.
        fold(
            &mut log,
            &vault,
            &under_vault(vec![emitted(&ev::ClaimPaid {
                claim_id: 7,
                assets: 60_000_000,
            })]),
        );
        let mut history = History::default();
        history.vaults.insert(vault, log);
        assert_eq!(cost(&history, &vault, &owner), Some(40.0));
        assert_eq!(cost(&history, &vault, &other), Some(30.0));
    }

    #[test]
    fn a_vault_with_nothing_in_it_is_at_a_dollar_a_share() {
        let (per_share, worth) = valued(1, 0, sv::state::OFFSET);
        assert!((per_share - 1.0).abs() < 1e-12);
        assert_eq!(worth, 0.0);
    }
}
