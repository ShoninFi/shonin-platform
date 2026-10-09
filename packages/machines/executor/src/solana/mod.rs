//! Serving the vaults of a Solana cluster.
//!
//! As on an EVM chain: each open vault whose bundle is in the store gets a
//! key of its own, made here from the enclave's randomness and held in
//! memory only. Its leaf joins the instance's attestation, and the keeper,
//! not this executor, sends the blessing and, once the registry vouches for
//! the key, its lamports: the executor holds no role account on Solana. Once
//! blessed and stocked, the key has the vault's station approve it, and the
//! vault's agent runs, its trades carried to the stand-in venue through the
//! station's guard. What running it costs is metered and signed for the
//! keeper, and how it runs is reported, signed with the same key.
//!
//! One executor at a time: a vault whose station approves another key that
//! the registry still counts as its executor is that executor's while it is
//! heard from. Once the API has had no report signed by that key for the
//! whole handover silence, the vault is taken over: this executor's key,
//! blessed, has the station approve it in the other's place. A key whose
//! image was revoked, or whose blessing lapsed, no longer holds a vault.

mod orca;
#[cfg(test)]
mod orca_on_a_cluster;
mod trading;
mod upkeep;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow};
use chain::SolanaServed as Served_;
use chain::solana::executor::Blessed;
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::vault::state::{Vault, VaultState};
use chain::solana::rate_limited;
use chain::solana::scan::VaultScan;
use chain::solana::{Keypair, Platform, Pubkey, Signer, SolanaClient};
use wasmtime::Engine;

use crate::attestation::{Attestation, SolanaServed};
use crate::handover;
use crate::health::Holds;
use crate::host::{Counters, Place};
use crate::usage::SignedThrough;
use crate::{AbortOnDrop, Args, Throttle};

pub use orca::DRIVER as ORCA;
use trading::Flights;
pub use trading::{Trading, tally_of};
pub use upkeep::Totals;

/// How often the cluster's vaults are looked at.
const SWEEP_EVERY: Duration = Duration::from_secs(2);

/// What a vault's key is given to pay for its sends, on a development
/// cluster and elsewhere, and below what it is topped back up.
const STAKE_DEVELOPMENT: u64 = 2_000_000_000;
const STAKE_PUBLIC: u64 = 50_000_000;

/// How often a served key's blessing and lamports are looked at. A blessing
/// lasts three days, a little over a minute on a development cluster's fast
/// clock, so it is looked at often.
const LOOK_EVERY: Duration = Duration::from_secs(3);

/// The longest a vault let go (closed, or held by another executor) keeps its
/// meter for its agent's last sends: as long as a send is watched, and two
/// minutes for the total after.
const LAST_SENDS_LAND_WITHIN: Duration =
    Duration::from_secs(chain::solana::client::LANDING_TIMEOUT.as_secs() + 2 * 60);

/// Whether every send of an agent stopped at `stopped` has landed or been
/// given up on, and a total counting up to then has been signed. A send
/// still landing adds its cost to the key's count, which only a total
/// signed after it lands bills.
fn last_sends_billed(flights: &Flights, signed_through: &SignedThrough, stopped: Instant) -> bool {
    let through = flights.last_settled().map_or(stopped, |at| at.max(stopped));
    !flights.any() && signed_through.covers(through)
}

/// Whether a vault let go at `stopped` may be dropped at `now`, its meter
/// with it: once its agent's last sends are billed, and never later than
/// [`LAST_SENDS_LAND_WITHIN`] after the stop.
fn may_drop(
    flights: &Flights,
    signed_through: &SignedThrough,
    stopped: Instant,
    now: Instant,
) -> bool {
    last_sends_billed(flights, signed_through, stopped)
        || now.saturating_duration_since(stopped) >= LAST_SENDS_LAND_WITHIN
}

/// Serve one cluster's vaults, for as long as the executor runs.
pub async fn run(
    served: Served_,
    native_usd: Option<f64>,
    engine: Engine,
    args: Arc<Args>,
    totals: Totals,
    attestation: Attestation,
) {
    // A simulated blessing is paid for by the keeper, which sends it.
    let payer = served
        .deployments
        .roles
        .as_ref()
        .and_then(|roles| roles.keeper.parse::<Pubkey>().ok());
    let Some(payer) = payer else {
        tracing::warn!(chain = %served.key, "the deployment names no keeper; its vaults are not served");
        return;
    };
    attestation.serve_solana(
        served.key.to_string(),
        SolanaServed {
            client: served.client.clone(),
            platform: served.platform.clone(),
            payer,
        },
    );
    let cluster = Cluster {
        attestation,
        chain_key: served.key.to_string(),
        vault_chain: served.vault_chain,
        client: served.client.clone(),
        platform: served.platform.clone(),
        native_usd,
        engine,
        args,
        totals,
        stake: if served.client.cluster == "localnet" {
            STAKE_DEVELOPMENT
        } else {
            STAKE_PUBLIC
        },
    };
    tracing::info!(cluster = %cluster.client.cluster, "serving the Solana vaults");
    let mut book = Book::default();
    let mut unswept = Throttle::default();
    let mut slowed = 0u32;
    loop {
        match cluster.sweep(&mut book).await {
            // A node that limits the rate failed at nothing: the next sweep
            // waits longer, the longer it keeps asking.
            Err(error) if rate_limited(&error) => {
                slowed = slowed.saturating_add(1);
                tracing::debug!(chain = %served.key, error = format!("{error:#}"), "the node limits the rate; the next sweep waits");
                tokio::time::sleep(crate::backoff(slowed)).await;
                continue;
            }
            Err(error) if unswept.due(Instant::now()) => {
                tracing::warn!(chain = %served.key, error = format!("{error:#}"), "could not look for work");
            }
            _ => {}
        }
        slowed = 0;
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

/// One cluster, and what serving its vaults needs.
#[derive(Clone)]
struct Cluster {
    attestation: Attestation,
    /// The cluster's chain key, as the attestation names it.
    chain_key: String,
    vault_chain: shared::Chain,
    client: SolanaClient,
    platform: Platform,
    native_usd: Option<f64>,
    engine: Engine,
    args: Arc<Args>,
    totals: Totals,
    stake: u64,
}

/// What a cluster's sweep keeps from one sweep to the next.
#[derive(Default)]
struct Book {
    running: HashMap<Pubkey, Served>,
    waiting_for_bundle: HashSet<Pubkey>,
    warned: HashMap<Pubkey, Throttle>,
    /// Vaults whose station approves another executor's key, still counted:
    /// watched for that key's reports.
    watching: HashMap<Pubkey, handover::Watch<Pubkey>>,
    /// Vaults another executor took from this one: never taken back while
    /// the key that took them is counted.
    replaced: HashSet<Pubkey>,
    /// The vaults found, the program listed only when its counts move.
    scan: Option<VaultScan>,
}

/// A vault key's leaf in the instance's attestation, taken out when the
/// vault is let go.
struct HeldLeaf {
    attestation: Attestation,
    cluster: String,
    vault: Pubkey,
    key: Pubkey,
}

impl Drop for HeldLeaf {
    fn drop(&mut self) {
        self.attestation
            .remove_solana(&self.cluster, self.vault, self.key);
    }
}

/// One vault taken on: its key, meter and reports, and its agent's serve,
/// started again whenever it ends while the vault still runs.
struct Served {
    shared: Shared,
    serving: Option<tokio::task::JoinHandle<Result<Stop>>>,
    /// When the next serve may start, after one that failed.
    retry_at: Option<Instant>,
    failures: u32,
    _upkeep: [AbortOnDrop; 2],
    _leaf: HeldLeaf,
    /// When the vault was let go (it closed, or another executor holds it)
    /// and its agent stopped. It is dropped, its meter with it, once every
    /// send of the agent's has landed or been given up on and a total
    /// counting them is signed; never later than [`LAST_SENDS_LAND_WITHIN`]
    /// after the stop.
    letting_go: Option<Instant>,
}

impl Served {
    /// Stop the vault's health reports, keeping its meter: a vault another
    /// executor holds is reported on by that executor alone, while the last
    /// bill, which the meter signs, is still to be collected.
    fn stop_reports(&self) {
        self.shared.holds.let_go();
        self._upkeep[1].0.abort();
    }

    /// Let the vault go, its agent having stopped because another key holds
    /// the station's approval or the key taken over from is heard from again:
    /// its reports stop at once, and it is dropped, its meter with it, once
    /// the agent's last sends are billed.
    fn let_go(&mut self) {
        self.stop_reports();
        self.letting_go = Some(Instant::now());
    }

    fn may_drop(&self, stopped: Instant) -> bool {
        may_drop(
            &self.shared.trading.flights,
            &self.shared.signed_through,
            stopped,
            Instant::now(),
        )
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(serving) = &self.serving {
            serving.abort();
        }
    }
}

/// What a vault's serve, meter and reports share.
#[derive(Clone)]
struct Shared {
    key: Arc<Keypair>,
    accounts: VaultAccounts,
    bundle: [u8; 32],
    trading: Arc<Trading>,
    dry: Arc<AtomicBool>,
    metered: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    counters: Arc<Mutex<Counters>>,
    account_events: crate::feed::Feed<Vec<u8>>,
    vault_events: crate::feed::Feed<crate::watch::VaultEvent>,
    failed_starts: Arc<AtomicU32>,
    /// The other executor's key, still counted, that this one takes the
    /// vault over from once its own key is blessed: the station's approval is
    /// taken from that key, and from no other, and only while it is still
    /// silent. Cleared once this key holds the approval; a serve started
    /// again after that finds another key in place only if that executor
    /// took the vault since, and lets it go.
    takes_over: Arc<Mutex<Option<handover::Takeover<Pubkey>>>>,
    /// Whether this serve holds the station's approval with the key, which
    /// alone lets the key's reports go.
    holds: Holds,
    /// The moment the meter's latest signed total counts up to.
    signed_through: SignedThrough,
}

/// Why a serve that didn't fail ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stop {
    /// Nothing is left to run.
    Finished,
    /// Another executor's key, still blessed, holds the station's approval.
    Replaced,
    /// The key this one was taking the vault over from reported again before
    /// this key took the approval: the vault is left to it, and its silence
    /// watched afresh.
    Resumed,
}

/// Whether a vault this executor holds no key for may be taken on, and from
/// which key.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TakeOn {
    /// Not now.
    Not,
    /// No other key the registry counts holds the station's approval.
    Free,
    /// From the key that has been silent for the whole watch.
    Over(handover::Takeover<Pubkey>),
}

/// Whether a key may take the station's approval while `held`, another key
/// the registry counts, holds it (or none does): only from no key, or from
/// the very key it takes the vault over from.
fn may_take_from(held: Option<Pubkey>, takes_over: Option<Pubkey>) -> bool {
    held.is_none_or(|held| takes_over == Some(held))
}

/// The key the station approves other than `key`, through any of its
/// drivers. Every approval withdraws all others, so there is at most one.
fn other_key(keys: &[Pubkey], key: Option<&Pubkey>) -> Option<Pubkey> {
    keys.iter()
        .copied()
        .find(|held| *held != Pubkey::default() && Some(held) != key)
}

/// Whether the station approves `key` through the driver it approves keys
/// through (the first), and no other key through any driver.
fn approved_alone(keys: &[Pubkey], key: &Pubkey) -> bool {
    keys.first() == Some(key) && other_key(keys, Some(key)).is_none()
}

impl Cluster {
    async fn sweep(&self, book: &mut Book) -> Result<()> {
        let scan = book
            .scan
            .get_or_insert_with(|| VaultScan::for_cluster(&self.client.cluster));
        let vaults = scan.look(&self.client, &self.platform).await?.vaults;
        let now = self.client.now().await?;
        for (address, vault) in vaults {
            if let Err(error) = self.sweep_one(book, address, &vault, now).await
                && !rate_limited(&error)
                && book.warned.entry(address).or_default().due(Instant::now())
            {
                tracing::warn!(vault = %address, error = format!("{error:#}"), "could not look at this vault");
            }
        }
        Ok(())
    }

    async fn sweep_one(
        &self,
        book: &mut Book,
        address: Pubkey,
        vault: &Vault,
        now: u64,
    ) -> Result<()> {
        // A closed vault is let go: its agent stopped at once, and its meter
        // kept until the agent's last sends have landed or been given up on
        // and a total counting their cost is signed.
        if matches!(vault.state, VaultState::Closed | VaultState::None) {
            if let Some(served) = book.running.get_mut(&address) {
                let stopped = *served.letting_go.get_or_insert_with(|| {
                    tracing::info!(vault = %address, "the vault has closed; letting it go once the agent's last sends are billed");
                    if let Some(serving) = served.serving.take() {
                        serving.abort();
                    }
                    Instant::now()
                });
                if served.may_drop(stopped) {
                    book.running.remove(&address);
                    tracing::info!(vault = %address, "the vault has closed; let go");
                }
            }
            book.waiting_for_bundle.remove(&address);
            return Ok(());
        }
        if let Some(served) = book.running.get_mut(&address) {
            if served
                .serving
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            {
                return Ok(());
            }
            if let Some(task) = served.serving.take() {
                match task.await {
                    Ok(Ok(Stop::Finished)) => {}
                    Ok(Ok(Stop::Replaced)) => {
                        tracing::info!(vault = %address, "another executor's key holds the station's approval; letting the vault go");
                        served.let_go();
                        book.replaced.insert(address);
                    }
                    Ok(Ok(Stop::Resumed)) => {
                        tracing::info!(vault = %address, "the key taken over from reported again; letting the vault go and watching it afresh");
                        served.let_go();
                    }
                    // Cut short by a node that limits the rate: served again
                    // after a pause, and not counted against the vault.
                    Ok(Err(error)) if rate_limited(&error) => {
                        served.retry_at = Some(Instant::now() + crate::backoff(1));
                        tracing::debug!(vault = %address, error = format!("{error:#}"), "the node limits the rate; serving this vault again shortly");
                    }
                    Ok(Err(error)) => {
                        served.failures += 1;
                        served.retry_at = Some(Instant::now() + crate::backoff(served.failures));
                        tracing::warn!(vault = %address, error = format!("{error:#}"), "stopped serving this vault");
                    }
                    Err(_) => {
                        served.failures += 1;
                        served.retry_at = Some(Instant::now() + crate::backoff(served.failures));
                    }
                }
            }
            if let Some(stopped) = served.letting_go {
                if served.may_drop(stopped) {
                    book.running.remove(&address);
                }
                return Ok(());
            }
            if served.retry_at.is_some_and(|at| Instant::now() < at) {
                return Ok(());
            }
        }
        // A retired vault runs until it is evicted; a killed or frozen one
        // runs through its halt's grace, for its agent to wind down in.
        let in_grace =
            vault.halted_at != 0 && now < vault.halted_at.saturating_add(vault.timers.halt_grace);
        let servable = match vault.state {
            VaultState::Open | VaultState::WindingDown => vault.halted_at == 0 || in_grace,
            VaultState::Killed | VaultState::Frozen => in_grace,
            _ => false,
        };
        if !servable {
            return Ok(());
        }
        let bundle = B256::from(vault.bundle_hash);
        if !crate::bundle_path(&self.args.store, bundle).exists() {
            if book.waiting_for_bundle.insert(address) {
                tracing::info!(vault = %address, %bundle, "no bundle in the store yet; the vault waits for it");
            }
            return Ok(());
        }
        book.waiting_for_bundle.remove(&address);

        let takes_over = if book.running.contains_key(&address) {
            None
        } else {
            match self
                .may_take_on(book, address, vault.strategy_id, &vault.bundle_hash)
                .await?
            {
                TakeOn::Not => return Ok(()),
                TakeOn::Free => None,
                TakeOn::Over(takeover) => Some(takeover),
            }
        };
        let served = match book.running.entry(address) {
            std::collections::hash_map::Entry::Occupied(served) => served.into_mut(),
            std::collections::hash_map::Entry::Vacant(empty) => {
                let served = self.take_on(address, vault, takes_over)?;
                tracing::info!(vault = %address, key = %served.shared.key.pubkey(), "taking on a vault");
                empty.insert(served)
            }
        };
        let cluster = self.clone();
        let shared = served.shared.clone();
        let strategy_id = vault.strategy_id;
        served.serving = Some(tokio::spawn(async move {
            cluster.serve(address, strategy_id, shared).await
        }));
        Ok(())
    }

    /// Whether a vault this executor holds no key for may be taken on: no
    /// other executor's key the registry counts holds its station's
    /// approval, or one does and has been silent for the whole handover
    /// silence, and then it is that key the approval is taken from. A vault
    /// another executor took from this one is taken on again only once no
    /// counted key holds it.
    async fn may_take_on(
        &self,
        book: &mut Book,
        vault: Pubkey,
        strategy_id: u64,
        bundle: &[u8; 32],
    ) -> Result<TakeOn> {
        let v = self.platform.vault_accounts(strategy_id);
        let Some(by) = self.held_by_another(&v, None, bundle).await? else {
            book.watching.remove(&vault);
            if book.replaced.remove(&vault) {
                tracing::info!(%vault, "no other executor's key holds the station's approval; taking the vault on again");
            }
            return Ok(TakeOn::Free);
        };
        if book.replaced.contains(&vault) {
            return Ok(TakeOn::Not);
        }
        let now = crate::unix_now();
        let watch = book.watching.entry(vault).or_insert_with(|| {
            tracing::info!(%vault, %by, "another executor's key holds the approval; watching for its reports");
            handover::Watch::new(by, now)
        });
        if watch.by != by {
            tracing::info!(%vault, %by, "another key holds the approval now; watching for its reports");
            *watch = handover::Watch::new(by, now);
        }
        if !watch.look_due(Instant::now()) {
            return Ok(TakeOn::Not);
        }
        let latest = handover::latest_solana_report(
            &crate::HTTP,
            &self.args.api_url,
            self.vault_chain,
            &self.client.cluster_value(),
            &vault,
        )
        .await;
        match latest {
            Ok(latest) => {
                if watch.heard(latest, crate::unix_now()) == handover::Heard::Report
                    || !watch.silent_for(self.args.handover_silence_seconds, crate::unix_now())
                {
                    return Ok(TakeOn::Not);
                }
            }
            Err(error) => {
                if book.warned.entry(vault).or_default().due(Instant::now()) {
                    tracing::warn!(%vault, error = format!("{error:#}"), "could not read the vault's latest report; the watch goes on");
                }
                return Ok(TakeOn::Not);
            }
        }
        tracing::info!(%vault, %by, silence = self.args.handover_silence_seconds, "the approved key has sent no report for the whole watch; taking the vault over");
        book.watching.remove(&vault);
        Ok(TakeOn::Over(handover::Takeover::new(by, crate::unix_now())))
    }

    /// Make the vault's key, and start its meter and its reports, which run
    /// for as long as the vault is served. `takes_over` is the other
    /// executor's key whose approval this one takes, once its own key is
    /// blessed.
    fn take_on(
        &self,
        address: Pubkey,
        vault: &Vault,
        takes_over: Option<handover::Takeover<Pubkey>>,
    ) -> Result<Served> {
        let key = Arc::new(crate::keys::fresh_solana()?);
        self.attestation.put_solana(
            &self.chain_key,
            self.client.cluster_value(),
            address,
            vault.bundle_hash,
            key.pubkey(),
        );
        let leaf = HeldLeaf {
            attestation: self.attestation.clone(),
            cluster: self.chain_key.clone(),
            vault: address,
            key: key.pubkey(),
        };
        let accounts = self.platform.vault_accounts(vault.strategy_id);
        if accounts.vault != address {
            return Err(anyhow!(
                "vault {address} is not at the address its strategy's id gives"
            ));
        }
        let spent = Arc::new(AtomicI64::new(0));
        let trading = Arc::new(Trading::new(
            self.client.clone(),
            self.platform.clone(),
            key.clone(),
            accounts.clone(),
            vault.bundle_hash,
            spent.clone(),
        ));
        let dry = Arc::new(AtomicBool::new(false));
        let metered = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Mutex::new(Counters::default()));
        let tank = upkeep::Tank {
            dry: dry.clone(),
            metered: metered.clone(),
            signed_through: Default::default(),
            other_cluster: Default::default(),
        };
        let metering = tokio::spawn(upkeep::meter(
            self.client.clone(),
            self.platform.clone(),
            key.clone(),
            address,
            self.native_usd,
            self.totals.clone(),
            tank.clone(),
            running.clone(),
            spent,
            self.args.ceilings,
        ));
        let holds = Holds::default();
        let reports = tokio::spawn(upkeep::report_on(upkeep::Reporting {
            client: self.client.clone(),
            platform: self.platform.clone(),
            key: key.clone(),
            vault: address,
            bundle: vault.bundle_hash,
            api: self.args.api_url.clone(),
            holds: holds.clone(),
            counters: counters.clone(),
            tank: tank.clone(),
            totals: self.totals.clone(),
            ceilings: self.args.ceilings,
            stake: self.stake,
        }));
        Ok(Served {
            shared: Shared {
                key,
                accounts,
                bundle: vault.bundle_hash,
                trading,
                dry,
                metered,
                running,
                counters,
                account_events: Default::default(),
                vault_events: Default::default(),
                failed_starts: Default::default(),
                takes_over: Arc::new(Mutex::new(takes_over)),
                holds,
                signed_through: tank.signed_through,
            },
            serving: None,
            retry_at: None,
            failures: 0,
            _upkeep: [AbortOnDrop(metering), AbortOnDrop(reports)],
            _leaf: leaf,
            letting_go: None,
        })
    }

    /// Bless the key, have the station approve it, and run the vault's agent,
    /// starting it again when it stops, until nothing is left to run or
    /// another executor holds the vault.
    async fn serve(&self, vault: Pubkey, strategy_id: u64, shared: Shared) -> Result<Stop> {
        let key = shared.key.clone();
        let bundle = shared.bundle;
        let v = shared.accounts.clone();
        let takeover = || shared.takes_over.lock().ok().and_then(|from| *from);

        // One executor at a time: a counted key in place is that executor's,
        // unless this one is taking the vault over from that very key. Once
        // this key has held the approval, another in its place took the
        // vault.
        if let Some(other) = self
            .held_by_another(&v, Some(&key.pubkey()), &bundle)
            .await?
            && !may_take_from(Some(other), takeover().map(|takeover| takeover.from))
        {
            tracing::info!(%vault, %other, "another executor's key holds the station's approval; not serving");
            return Ok(Stop::Replaced);
        }

        // The keeper blesses the key and, once the registry vouches for it,
        // gives it lamports; then it takes the station's approval, in the
        // place of a key that went silent if it must.
        loop {
            if self.finished(&vault).await? {
                return Ok(Stop::Finished);
            }
            let blessed = self.note_blessing(&key.pubkey(), &vault, &bundle).await?;
            let stocked = self.client.lamports(&key.pubkey()).await? >= self.stake / 2;
            if blessed && stocked {
                break;
            }
            tokio::time::sleep(LOOK_EVERY).await;
        }
        tracing::info!(%vault, key = %key.pubkey(), "the key is blessed and holds its lamports");
        // The blessing can take a while: where the approval stands is read
        // again now, and it is taken only from no counted key, or from the
        // key this one takes over from while that key is still silent.
        if let Some(stop) = self.approved(&v, &key, &bundle, takeover()).await? {
            return Ok(stop);
        }
        // The approval is this key's now: a key that takes it from here on
        // has replaced this executor, which lets the vault go. Its reports
        // may go from here on.
        if let Ok(mut from) = shared.takes_over.lock() {
            *from = None;
        }
        shared.holds.take();

        // Kept blessed and stocked for as long as the serve runs.
        let replaced = Arc::new(AtomicBool::new(false));
        let _keeping = AbortOnDrop(tokio::spawn({
            let cluster = self.clone();
            let key = key.clone();
            let replaced = replaced.clone();
            let v = v.clone();
            async move { cluster.keep_blessed(key, v, bundle, replaced).await }
        }));

        // The manifest the builder recorded, as the registry holds its hash.
        let registered = self
            .platform
            .strategy(&self.client, strategy_id)
            .await?
            .ok_or_else(|| anyhow!("strategy {strategy_id} is not registered"))?;
        let recorded = crate::recorded_in(
            &self.args.store,
            B256::from(registered.bundle_hash),
            B256::from(registered.manifest_hash),
            B256::from(bundle),
            &self.args.chain_args.network.to_string(),
        )?;
        // Which half serves a driver is told by its name in the recorded
        // manifest, as on the EVM: the Orca half works under the name the
        // station pinned it under, and refuses to start unless the station
        // pinned the Orca driver this image lists under that name.
        let orca = recorded.drivers.iter().any(|name| name == orca::DRIVER);
        if orca {
            shared
                .trading
                .take_up_orca(shared.account_events.sender())
                .await?;
        }

        loop {
            if replaced.load(Ordering::SeqCst) {
                return Ok(Stop::Replaced);
            }
            if self.finished(&vault).await? {
                tracing::info!(%vault, "the vault has stopped; letting the agent go");
                return Ok(Stop::Finished);
            }
            // A dry tank, or one not yet read, holds the agent back.
            if shared.dry.load(Ordering::SeqCst) || !shared.metered.load(Ordering::SeqCst) {
                tokio::time::sleep(SWEEP_EVERY).await;
                continue;
            }
            let account = self.platform.vault(&self.client, &vault).await?;
            let allowances = self
                .args
                .ceilings
                .hold(&upkeep::allowances(&account.allowances));
            let component = crate::component_of(&self.args.store, B256::from(bundle), &recorded)
                .inspect_err(|error| {
                    if error.is::<crate::NotTheBundle>()
                        && let Ok(mut counters) = shared.counters.lock()
                    {
                        counters.not_the_bundle += 1;
                    }
                })?;
            if let Ok(mut counters) = shared.counters.lock() {
                counters.not_the_bundle = 0;
            }
            // The markets this start of the agent follows, as it asks about
            // them and trades them.
            let following = crate::watch::Following::default();
            let watching = AbortOnDrop(tokio::spawn(trading::watch(
                self.client.clone(),
                self.platform.clone(),
                vault,
                !orca,
                following.clone(),
                shared.account_events.sender(),
                shared.vault_events.sender(),
            )));
            let _prices = orca.then(|| {
                AbortOnDrop(tokio::spawn(orca::watch_prices(
                    self.client.clone(),
                    shared.account_events.sender(),
                )))
            });
            let halting_heard = Arc::new(AtomicBool::new(false));
            let instantiated = Arc::new(std::sync::OnceLock::new());
            let runner = crate::runner::Runner {
                place: Place::Solana(shared.trading.clone()),
                home_chain: shared::Chain::Solana.name().to_string(),
                drivers: recorded.drivers.clone(),
                models: None,
                network: recorded.network,
                allowances,
                halting_heard: halting_heard.clone(),
                running: shared.running.clone(),
                instantiate_within: crate::runner::INSTANTIATE_WITHIN,
                instantiated: instantiated.clone(),
                following,
            };
            let mut ended_cleanly = false;
            tokio::select! {
                outcome = runner.run(
                    &self.engine,
                    &component,
                    shared.account_events.reader(),
                    shared.vault_events.reader(),
                    shared.counters.clone(),
                ) => match outcome {
                    Ok(stopped) => ended_cleanly = stopped == crate::runner::Stopped::Finished,
                    Err(error) => tracing::warn!(%vault, error = format!("{error:#}"), "the agent could not be run"),
                },
                _ = crate::until(shared.dry.clone()) => {}
                _ = crate::until(replaced.clone()) => {}
                _ = self.until_finished(&vault) => {}
            }
            drop(watching);
            if instantiated
                .get()
                .is_some_and(|at| at.elapsed() >= crate::RAN_FOR_A_WHILE)
            {
                shared.failed_starts.store(0, Ordering::SeqCst);
            }
            // An agent told of the halt that ended cleanly has wound down:
            // the grace is waited out rather than the agent started again.
            if ended_cleanly && halting_heard.load(Ordering::SeqCst) {
                while !self.finished(&vault).await? {
                    if replaced.load(Ordering::SeqCst) {
                        return Ok(Stop::Replaced);
                    }
                    tokio::time::sleep(SWEEP_EVERY).await;
                }
                return Ok(Stop::Finished);
            }
            if replaced.load(Ordering::SeqCst)
                || shared.dry.load(Ordering::SeqCst)
                || self.finished(&vault).await?
            {
                continue;
            }
            if let Ok(mut counters) = shared.counters.lock() {
                counters.restarts += 1;
            }
            let failed = shared.failed_starts.fetch_add(1, Ordering::SeqCst) + 1;
            tracing::warn!(%vault, "restarting the strategy");
            tokio::time::sleep(crate::backoff(failed)).await;
        }
    }

    /// Whether nothing is left to run: the vault has closed, or it halted
    /// and its grace is over.
    async fn finished(&self, vault: &Pubkey) -> Result<bool> {
        let account = self.platform.vault(&self.client, vault).await?;
        if matches!(account.state, VaultState::Closed | VaultState::None) {
            return Ok(true);
        }
        if account.halted_at == 0 {
            return Ok(false);
        }
        Ok(self.client.now().await? >= account.halted_at.saturating_add(account.timers.halt_grace))
    }

    async fn until_finished(&self, vault: &Pubkey) {
        loop {
            if matches!(self.finished(vault).await, Ok(true)) {
                return;
            }
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    }

    /// Another key the station approves that the registry still counts as
    /// this vault's executor, if there is one: any approved key but `key`.
    async fn held_by_another(
        &self,
        v: &VaultAccounts,
        key: Option<&Pubkey>,
        bundle: &[u8; 32],
    ) -> Result<Option<Pubkey>> {
        let station = self.platform.station(&self.client, &v.station).await?;
        let keys: Vec<Pubkey> = station.drivers.iter().map(|driver| driver.key).collect();
        let Some(approved) = other_key(&keys, key) else {
            return Ok(None);
        };
        Ok(self.counts(&approved, v, bundle).await?.then_some(approved))
    }

    /// Whether the registry counts `key` as this vault's executor.
    async fn counts(&self, key: &Pubkey, v: &VaultAccounts, bundle: &[u8; 32]) -> Result<bool> {
        let blessed = Blessed::read(&self.client, &self.platform, key, &v.vault, bundle).await?;
        Ok(blessed.holds(self.client.now().await?))
    }

    /// Read where the key's blessing for the vault stands, and tell the
    /// attestation whether it wants a fresh one. Says whether it holds.
    async fn note_blessing(&self, key: &Pubkey, vault: &Pubkey, bundle: &[u8; 32]) -> Result<bool> {
        let now = self.client.now().await?;
        let blessed = Blessed::read(&self.client, &self.platform, key, vault, bundle).await?;
        self.attestation.note_solana(
            &self.chain_key,
            *vault,
            *key,
            blessed.due(now),
            blessed.until,
            blessed.ttl,
        );
        Ok(blessed.holds(now))
    }

    /// Have the station approve the key, if it doesn't already: in the place
    /// of no key the registry counts, or of the key `takes_over` names while
    /// that key is still silent. The station lets any blessed key displace
    /// another, so this is the rule that keeps one executor at a time. Says
    /// why the serve stops if the key is not to be approved.
    async fn approved(
        &self,
        v: &VaultAccounts,
        key: &Keypair,
        bundle: &[u8; 32],
        takes_over: Option<handover::Takeover<Pubkey>>,
    ) -> Result<Option<Stop>> {
        let station = self.platform.station(&self.client, &v.station).await?;
        let keys: Vec<Pubkey> = station.drivers.iter().map(|driver| driver.key).collect();
        if approved_alone(&keys, &key.pubkey()) {
            return Ok(None);
        }
        if let Some(other) = other_key(&keys, Some(&key.pubkey()))
            && self.counts(&other, v, bundle).await?
        {
            let takeover = match takes_over {
                Some(takeover) if may_take_from(Some(other), Some(takeover.from)) => takeover,
                _ => {
                    tracing::info!(vault = %v.vault, %other, "another executor's key holds the station's approval; not serving");
                    return Ok(Some(Stop::Replaced));
                }
            };
            // An API that can't be asked is no silence: the serve fails, and
            // is tried again.
            let latest = handover::latest_solana_report(
                &crate::HTTP,
                &self.args.api_url,
                self.vault_chain,
                &self.client.cluster_value(),
                &v.vault,
            )
            .await?;
            if !takeover.still_silent(latest, crate::unix_now()) {
                tracing::info!(vault = %v.vault, from = %other, "the key taken over from has reported again; leaving it the vault");
                return Ok(Some(Stop::Resumed));
            }
        }
        let ix = self
            .platform
            .ix_approve_key(&self.client, v, bundle, &key.pubkey(), 0, &key.pubkey())
            .await?;
        self.client
            .send(vec![ix], &[key])
            .await
            .context("the station would not approve the key")?;
        tracing::info!(vault = %v.vault, key = %key.pubkey(), "approved at the station");
        Ok(None)
    }

    /// Keep the attestation told where the key's blessing stands, and raise
    /// `replaced` once another executor's key, still counted, holds the
    /// station's approval.
    async fn keep_blessed(
        &self,
        key: Arc<Keypair>,
        v: VaultAccounts,
        bundle: [u8; 32],
        replaced: Arc<AtomicBool>,
    ) {
        let mut said = Throttle::default();
        loop {
            tokio::time::sleep(LOOK_EVERY).await;
            let kept = async {
                if let Some(other) = self
                    .held_by_another(&v, Some(&key.pubkey()), &bundle)
                    .await?
                {
                    tracing::info!(vault = %v.vault, %other, "another executor's key holds the station's approval");
                    replaced.store(true, Ordering::SeqCst);
                    return anyhow::Ok(true);
                }
                // The keeper renews the blessing and the lamports; a key
                // that no longer holds isn't approved again until it does.
                if !self.note_blessing(&key.pubkey(), &v.vault, &bundle).await? {
                    return anyhow::Ok(false);
                }
                // A key the station dropped (another's approval that lapsed
                // and was then replaced) is approved again, in the place of
                // no counted key.
                if self.approved(&v, &key, &bundle, None).await?.is_some() {
                    replaced.store(true, Ordering::SeqCst);
                    return anyhow::Ok(true);
                }
                anyhow::Ok(false)
            };
            match kept.await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    if said.due(Instant::now()) {
                        tracing::warn!(vault = %v.vault, error = format!("{error:#}"), "could not keep the key blessed; trying again");
                    }
                }
            }
        }
    }
}

/// A cluster that answers who it is (devnet) and counts every other
/// question, each of which it turns away as rate-limited.
#[cfg(test)]
pub(crate) async fn counting_cluster() -> (
    SolanaClient,
    Arc<std::sync::atomic::AtomicUsize>,
    AbortOnDrop,
) {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = asked.clone();
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
            let counting = counting.clone();
            async move {
                let id = request["id"].clone();
                if request["method"] == "getGenesisHash" {
                    return (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"
                        })),
                    );
                }
                counting.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": 429, "message": "Too many requests"}
                    })),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let key: chain::ChainKey = "solana:devnet".parse().unwrap();
    let config: chain::ChainConfig = serde_json::from_value(serde_json::json!({
        "label": "Solana devnet (stand-in)",
        "rpc": url,
        "dollar": "4Gw9TmkKDvnGtBcMpJwR4rRDEWc6KVujegc1j4ebtHTK",
    }))
    .unwrap();
    let client = SolanaClient::connect_with(&key, &config, None)
        .await
        .unwrap();
    (client, asked, server)
}

/// The platform on devnet, as a stand-in names it.
#[cfg(test)]
pub(crate) fn devnet_platform() -> Platform {
    Platform {
        cluster_name: "devnet".into(),
        cluster: chain::solana::programs::common::digest::cluster_value("devnet"),
        mint: Pubkey::new_from_array([1; 32]),
        admin: Pubkey::new_from_array([2; 32]),
        platform: Pubkey::new_from_array([3; 32]),
        operator: Pubkey::new_from_array([4; 32]),
        checker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault let go because another key holds the station's approval, or
    /// because the key taken over from is heard from again, stops reporting
    /// at once, its key no longer counted as holding the approval, while its
    /// meter runs on for the agent's last bill.
    #[tokio::test]
    async fn a_solana_vault_let_go_stops_its_reports_at_once_and_keeps_its_meter() {
        let (client, _asked, _cluster) = counting_cluster().await;
        let platform = devnet_platform();
        let key = Arc::new(Keypair::new_from_array([7; 32]));
        let accounts = platform.vault_accounts(1);
        let holds = Holds::default();
        holds.take();
        let never = || AbortOnDrop(tokio::spawn(std::future::pending::<()>()));
        let attestation = Attestation::new(crate::attestation::Mode::Development).unwrap();
        let mut served = Served {
            shared: Shared {
                key: key.clone(),
                accounts: accounts.clone(),
                bundle: [5; 32],
                trading: Arc::new(Trading::new(
                    client,
                    platform,
                    key.clone(),
                    accounts.clone(),
                    [5; 32],
                    Default::default(),
                )),
                dry: Default::default(),
                metered: Default::default(),
                running: Default::default(),
                counters: Default::default(),
                account_events: Default::default(),
                vault_events: Default::default(),
                failed_starts: Default::default(),
                takes_over: Default::default(),
                holds: holds.clone(),
                signed_through: Default::default(),
            },
            serving: None,
            retry_at: None,
            failures: 0,
            _upkeep: [never(), never()],
            _leaf: HeldLeaf {
                attestation,
                cluster: "solana:devnet".into(),
                vault: accounts.vault,
                key: key.pubkey(),
            },
            letting_go: None,
        };
        served.let_go();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!holds.holds());
        assert!(served._upkeep[1].0.is_finished(), "the reports go on");
        assert!(!served._upkeep[0].0.is_finished(), "the meter stopped");
        assert!(served.letting_go.is_some());
    }

    /// The station lets any blessed key displace another, so the executor's
    /// rule is what keeps one executor at a time: a key takes the approval
    /// from no counted key, or from the key it takes the vault over from, and
    /// from no other, whichever came in meanwhile.
    #[test]
    fn a_takeover_takes_the_approval_only_from_the_key_it_watched_go_silent() {
        let watched = Pubkey::new_from_array([0xaa; 32]);
        let third = Pubkey::new_from_array([0xcc; 32]);
        assert!(may_take_from(None, None));
        assert!(may_take_from(None, Some(watched)));
        assert!(may_take_from(Some(watched), Some(watched)));
        assert!(!may_take_from(Some(third), Some(watched)));
        assert!(!may_take_from(Some(watched), None));
        assert!(!may_take_from(Some(third), None));
    }

    /// Whether another key holds the station, and whether this key holds it
    /// alone, are read from every driver alike.
    #[test]
    fn the_key_in_place_is_read_from_every_driver_of_the_station() {
        let mine = Pubkey::new_from_array([0x11; 32]);
        let other = Pubkey::new_from_array([0x22; 32]);
        let none = Pubkey::default();
        assert_eq!(other_key(&[none, none], Some(&mine)), None);
        assert_eq!(other_key(&[mine, none], Some(&mine)), None);
        assert_eq!(other_key(&[none, other], Some(&mine)), Some(other));
        assert_eq!(other_key(&[mine, other], Some(&mine)), Some(other));
        assert_eq!(other_key(&[other, none], None), Some(other));
        assert!(approved_alone(&[mine, none], &mine));
        assert!(!approved_alone(&[mine, other], &mine));
        assert!(!approved_alone(&[none, mine], &mine));
        assert!(!approved_alone(&[], &mine));
    }

    /// A vault let go is kept while a send of its agent's may still land,
    /// and until a total signed after the send settled counts its cost.
    #[test]
    fn a_vault_let_go_is_kept_until_its_agent_s_last_sends_are_billed() {
        let flights = Flights::default();
        let signed = SignedThrough::default();
        let stopped = Instant::now();

        // A send still landing: kept, however recent the total.
        let send = flights.start();
        signed.signed(Instant::now());
        assert!(!may_drop(&flights, &signed, stopped, Instant::now()));
        assert!(!may_drop(
            &flights,
            &signed,
            stopped,
            stopped + Duration::from_secs(60)
        ));

        // It lands: kept until a total counts up to then.
        drop(send);
        assert!(!may_drop(&flights, &signed, stopped, Instant::now()));
        signed.signed(Instant::now());
        assert!(may_drop(&flights, &signed, stopped, Instant::now()));
    }

    #[test]
    fn a_vault_let_go_is_dropped_at_the_latest_once_its_sends_could_have_landed() {
        let flights = Flights::default();
        let signed = SignedThrough::default();
        let stopped = Instant::now();
        let _stuck = flights.start();
        assert!(!may_drop(
            &flights,
            &signed,
            stopped,
            stopped + LAST_SENDS_LAND_WITHIN - Duration::from_secs(1)
        ));
        assert!(may_drop(
            &flights,
            &signed,
            stopped,
            stopped + LAST_SENDS_LAND_WITHIN
        ));
        // Longer than a send is watched for.
        assert!(LAST_SENDS_LAND_WITHIN > chain::solana::client::LANDING_TIMEOUT);
    }

    /// A send is watched in a task of its own: stopping the agent while it
    /// waits leaves the send in flight until its watch ends.
    #[tokio::test]
    async fn a_send_outlives_the_agent_stopped_while_it_waits() {
        let flights = Flights::default();
        let (tell, heard) = tokio::sync::oneshot::channel::<()>();
        let agent = tokio::spawn({
            let flights = flights.clone();
            async move {
                let _ = flights
                    .watched(async move {
                        let _ = heard.await;
                    })
                    .await;
            }
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        agent.abort();
        let _ = agent.await;
        assert!(flights.any(), "the send is still in flight");
        let _ = tell.send(());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!flights.any());
        assert!(flights.last_settled().is_some());
    }
}
