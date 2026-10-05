//! Serving the vaults of a Solana cluster.
//!
//! As on an EVM chain: each open vault whose bundle is in the store gets a
//! key of its own, made here from the image and the vault; the key is given
//! lamports, blessed for the vault and its bundle (with the development
//! seal on a development cluster) and blessed again before half its
//! blessing has gone; the vault's station approves it; and the vault's
//! agent runs, its trades carried to the stand-in venue through the
//! station's guard. What running it costs is metered and signed for the
//! keeper, and how it runs is reported, signed with the same key.
//!
//! One executor at a time: a vault whose station approves another key that
//! is still blessed as its executor is left to that key.

mod trading;
mod upkeep;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow};
use chain::solana::executor::{self as on_chain, Blessed};
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::vault::state::{Vault, VaultState};
use chain::solana::{Keypair, Platform, Pubkey, Signer, SolanaClient};
use chain::{Role, SolanaServed};
use wasmtime::Engine;

use crate::host::{Counters, Place};
use crate::usage::SignedThrough;
use crate::{AbortOnDrop, Args, Throttle};

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
    served: SolanaServed,
    native_usd: Option<f64>,
    engine: Engine,
    args: Arc<Args>,
    totals: Totals,
) {
    let cluster = Cluster {
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
    loop {
        if let Err(error) = cluster.sweep(&mut book).await
            && unswept.due(Instant::now())
        {
            tracing::warn!(chain = %served.key, error = format!("{error:#}"), "could not look for work");
        }
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

/// One cluster, and what serving its vaults needs.
#[derive(Clone)]
struct Cluster {
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
    /// When the vault was let go (it closed, or another executor holds it)
    /// and its agent stopped. It is dropped, its meter with it, once every
    /// send of the agent's has landed or been given up on and a total
    /// counting them is signed; never later than [`LAST_SENDS_LAND_WITHIN`]
    /// after the stop.
    letting_go: Option<Instant>,
}

impl Served {
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
}

impl Cluster {
    async fn sweep(&self, book: &mut Book) -> Result<()> {
        let vaults = on_chain::vaults(&self.client).await?;
        let now = self.client.now().await?;
        for (address, vault) in vaults {
            if let Err(error) = self.sweep_one(book, address, &vault, now).await
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
                        served.letting_go = Some(Instant::now());
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

        let served = match book.running.entry(address) {
            std::collections::hash_map::Entry::Occupied(served) => served.into_mut(),
            std::collections::hash_map::Entry::Vacant(empty) => {
                let served = self.take_on(address, vault)?;
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

    /// Make the vault's key, and start its meter and its reports, which run
    /// for as long as the vault is served.
    fn take_on(&self, address: Pubkey, vault: &Vault) -> Result<Served> {
        let key = Arc::new(on_chain::vault_key(&address));
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
        let reports = tokio::spawn(upkeep::report_on(
            self.client.clone(),
            self.platform.clone(),
            key.clone(),
            address,
            vault.bundle_hash,
            self.args.api_url.clone(),
            counters.clone(),
            tank.clone(),
            self.totals.clone(),
            self.args.ceilings,
            self.stake,
        ));
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
                signed_through: tank.signed_through,
            },
            serving: None,
            retry_at: None,
            failures: 0,
            _upkeep: [AbortOnDrop(metering), AbortOnDrop(reports)],
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

        // One executor at a time.
        if let Some(other) = self.held_by_another(&v, &key.pubkey(), &bundle).await? {
            tracing::info!(%vault, %other, "another executor's key holds the station's approval; not serving");
            return Ok(Stop::Replaced);
        }
        self.stocked(&key.pubkey()).await?;
        self.blessed(&key, &vault, &bundle).await?;
        self.approved(&v, &key, &bundle).await?;

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
            let watching = AbortOnDrop(tokio::spawn(trading::watch(
                self.client.clone(),
                self.platform.clone(),
                vault,
                shared.account_events.sender(),
                shared.vault_events.sender(),
            )));
            let halting_heard = Arc::new(AtomicBool::new(false));
            let instantiated = Arc::new(std::sync::OnceLock::new());
            let runner = crate::runner::Runner {
                place: Place::Solana(shared.trading.clone()),
                home_chain: shared::Chain::Solana.name().to_string(),
                driver: recorded.driver.clone(),
                network: recorded.network,
                allowances,
                halting_heard: halting_heard.clone(),
                running: shared.running.clone(),
                instantiate_within: crate::runner::INSTANTIATE_WITHIN,
                instantiated: instantiated.clone(),
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

    /// Another key the station approves that is still blessed as this
    /// vault's executor, if there is one.
    async fn held_by_another(
        &self,
        v: &VaultAccounts,
        key: &Pubkey,
        bundle: &[u8; 32],
    ) -> Result<Option<Pubkey>> {
        let station = self.platform.station(&self.client, &v.station).await?;
        let approved = station
            .drivers
            .first()
            .map(|driver| driver.key)
            .unwrap_or_default();
        if approved == Pubkey::default() || approved == *key {
            return Ok(None);
        }
        let other =
            Blessed::read(&self.client, &self.platform, &approved, &v.vault, bundle).await?;
        Ok(other.holds(self.client.now().await?).then_some(approved))
    }

    /// Give the key lamports when it holds less than half its stake: it
    /// pays for its blessing, its approval and the agent's sends. The
    /// platform's executor account pays.
    async fn stocked(&self, key: &Pubkey) -> Result<()> {
        let holds = self.client.lamports(key).await?;
        if holds >= self.stake / 2 {
            return Ok(());
        }
        let payer = self.client.role(Role::Executor)?;
        self.client
            .send(
                vec![on_chain::ix_fund(&payer.pubkey(), key, self.stake - holds)],
                &[payer],
            )
            .await
            .context("giving the key lamports")?;
        Ok(())
    }

    /// Bless the key for the vault and its bundle unless it is blessed with
    /// more than half its blessing left. On a development cluster the
    /// development attestor seals the attestation; a cluster with no
    /// attestor key here can't be served yet.
    async fn blessed(&self, key: &Keypair, vault: &Pubkey, bundle: &[u8; 32]) -> Result<bool> {
        let now = self.client.now().await?;
        let blessed =
            Blessed::read(&self.client, &self.platform, &key.pubkey(), vault, bundle).await?;
        if blessed.for_a_while(now) {
            return Ok(false);
        }
        let attestor = self
            .client
            .role(Role::Attestor)
            .context("no attestor key to seal the attestation with")?;
        self.platform
            .bless_executor(&self.client, key, attestor, &key.pubkey(), vault, bundle)
            .await
            .context("blessing the key")?;
        let until = Blessed::read(&self.client, &self.platform, &key.pubkey(), vault, bundle)
            .await?
            .until;
        tracing::info!(%vault, key = %key.pubkey(), until, "blessed");
        Ok(true)
    }

    /// Have the station approve the key, if it doesn't already.
    async fn approved(&self, v: &VaultAccounts, key: &Keypair, bundle: &[u8; 32]) -> Result<()> {
        let station = self.platform.station(&self.client, &v.station).await?;
        if station
            .drivers
            .first()
            .is_some_and(|driver| driver.key == key.pubkey())
        {
            return Ok(());
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
        Ok(())
    }

    /// Keep the key blessed and stocked, and raise `replaced` once another
    /// executor's key, still blessed, holds the station's approval.
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
                if let Some(other) = self.held_by_another(&v, &key.pubkey(), &bundle).await? {
                    tracing::info!(vault = %v.vault, %other, "another executor's key holds the station's approval");
                    replaced.store(true, Ordering::SeqCst);
                    return anyhow::Ok(true);
                }
                self.stocked(&key.pubkey()).await?;
                self.blessed(&key, &v.vault, &bundle).await?;
                // A key the station dropped (another's approval that lapsed
                // and was then replaced) is approved again.
                self.approved(&v, &key, &bundle).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
