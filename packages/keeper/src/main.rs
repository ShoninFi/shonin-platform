//! The keeper: the clockwork nobody has to trust.
//!
//! Once a chain-day it collects each vault's bill from its tank, with the
//! running total the executor signed, and at once when the author asks to
//! withdraw the tank without one. It values a vault in a slot only when
//! something waits for a valuation (a request to price, the weekly payday, a
//! claim or units held off by a close that has ended, a halted vault's
//! close), and otherwise once a day, and settles what the valuation prices:
//! it is the only one that does, so two loops never race each other for the
//! same valuation. Each valuation is simulated first at the moment it would
//! land and a block later, and is not sent if either would revert. It tries
//! every claim that
//! is waiting, at most once a slot and less often the longer it keeps
//! waiting, and not while the market is outside the band, serves units left
//! by the pro-rata fallback,
//! freezes a vault whose claim or units have gone unserved too long (a claim
//! only once it has been tried that round and is still unpaid), evicts a
//! retired vault whose wind-down is over and cancels one that never opened.
//! It sends nothing that would do nothing: no settle with nothing owed, no
//! unwind that would neither bring anything home nor empty the station, no
//! second unwind in a slot, no daily `keep` with nothing to collect and no
//! fill to queue, and no valuation while the station reads as outside the
//! band, which would be void. What must land inside a slot's window (a
//! valuation, the one-transaction close) is sent only when it will land
//! there, judged by the slot the next block falls in. It takes back a venue
//! approval from a key whose blessing has gone or whose driver was revoked,
//! and one a station's skipped revoke left in place, closed and cancelled
//! vaults included, and keeps each vault's restricted list current. It keeps
//! the vaults of every factory the deployment's factory replaces as well as
//! its own. It collects a halted vault's bill when it first sees the
//! halt and again once the halt's grace is over (the usage cap is
//! cumulative, so waiting loses nothing), and always before it sends
//! anything that may close the vault, whose author may then take the tank at
//! once, and before an escape, halted or not. The executor is asked off the
//! round (once a day for an open vault, and at each of those points), so an
//! executor that hangs costs the round nothing, and each newer total it
//! answers with is collected in one `keep`, in a round after. Before a
//! `close` it is asked once more and its answer waited for a few seconds;
//! the one-transaction close waits only for an ask already on its way, and
//! is judged again after it. Once the halt's grace is
//! over it stops the station and unwinds it
//! before the slot's valuation, so the same round can value the empty
//! station and close the vault. Every unwind of a stopped station is
//! simulated before it is sent: the one-transaction close only if it would
//! go through, and an unwind of its own only if it would bring home a cent
//! or more than the vault's dust, whichever is larger, or empty the station.
//! A simulation that finds it not worth sending is made again less and less
//! often, up to every five minutes; simulations are reads, and cost nothing.
//! Nothing is sent that hasn't been simulated: a chain whose node can't run
//! the simulation isn't kept until it can, and a simulation that fails is
//! made again next slot.
//! A closed vault still has whatever bill the executor signs for it
//! collected, its station stopped once the grace is over and asked to send
//! home whatever it still holds; one left with nothing owed and an empty,
//! stopped station is looked at once a day. No bill is collected from an
//! empty tank, which would write it off, nor once a collection has reached
//! the halt's grace end, after which the vault charges nothing.
//!
//! What it remembers having done is remembered only once the transaction was
//! mined and went through: one refused or reverted is tried again.
//!
//! Its health (`/health`, on `--port`) answers 503 while any chain it is to
//! keep has not connected, has no keeper account, has a node that can't
//! simulate, or has not finished a round lately; while a call it still tries
//! does not go out; while a vault's unwind can't be simulated or its tank
//! can't be read; or while the executor does not answer, or answers 404 for
//! every vault.
//!
//! Every one of those calls is public: anyone may send them, and a depositor
//! who does not want to wait can send their own. The keeper exists so that
//! nobody has to.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256};
use anyhow::{Context, Result};
use chain::bindings::closer::ICloser;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{ClaimState, IVault, RequestKind, Tank, ValuationStatus, VaultState};
use chain::{Client, Deployments, Role};
use clap::Parser;
use serde::Deserialize;
use shared::Chain;

#[derive(Parser)]
#[command(about = "Keeps each vault's clockwork turning")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    /// Accepted for the services' common arguments; the keeper reads no
    /// store.
    #[arg(long)]
    store: Option<PathBuf>,
    /// Accepted for the services' common arguments; the keeper asks the API
    /// nothing.
    #[arg(long)]
    api_url: Option<String>,
    /// Where the executor serves each vault's signed running total, and what
    /// it publishes about its attestation: the keys it holds and the seal
    /// that proves them, which the keeper relays to each chain's registry.
    #[arg(long, env = "SHONIN_EXECUTOR_URL")]
    executor_url: String,
    /// Where the builder and the reviewer publish their attestation, in the
    /// executor's shape: their keys' blessings are relayed and their keys
    /// stocked with gas as the executor's are. Either may be off; that is
    /// normal, and nothing is said above debug.
    #[arg(long, env = "SHONIN_SERVICE_ATTESTATION_URLS", value_delimiter = ',')]
    service_attestation_urls: Vec<String>,
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// The address the health route listens on: 0.0.0.0 in a container.
    #[arg(long, default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
    /// How long, in seconds of the keeper's own clock, a halted vault's
    /// close is held for its bill and a closed vault's last bill is waited
    /// for; as long in chain time is waited as well. Shortened only by tests
    /// that run the keeper as a process.
    #[arg(long, hide = true, default_value_t = MOST_CLOSE_HOLD)]
    own_clock_hold: u64,
    /// How long, in seconds of the keeper's own clock, the executor may
    /// answer 404 for every vault it is asked about, serving none, before the
    /// keeper's health says so. Shortened only by tests that run the keeper
    /// as a process.
    #[arg(long, hide = true, default_value_t = 60 * MINUTE)]
    serving_none_for: u64,
}

/// [`Args::own_clock_hold`], once read.
static OWN_CLOCK_HOLD: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

/// [`Args::serving_none_for`], once read.
static SERVING_NONE_FOR: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

/// How long an executor may serve no vault before it is said (see
/// [`Args::serving_none_for`]).
fn serving_none_for() -> Duration {
    *SERVING_NONE_FOR.get_or_init(|| Duration::from_secs(60 * MINUTE))
}

/// How long a hold or a closed vault's last bill's window lasts on the
/// keeper's own clock (see [`Args::own_clock_hold`]).
fn own_clock_hold() -> Duration {
    *OWN_CLOCK_HOLD.get_or_init(|| Duration::from_secs(MOST_CLOSE_HOLD))
}

/// Whether a hold, or a closed vault's last bill's window, that began at
/// `since` (chain time, and on the keeper's clock) is over: [`LAST_ORDERS`]
/// on both clocks, so a chain whose clock runs fast (the devnet moves an
/// hour a block) can't cut it short.
fn waited(since: (u64, std::time::Instant), now: u64) -> bool {
    now >= since.0.saturating_add(LAST_ORDERS) && since.1.elapsed() >= own_clock_hold()
}

/// A running total as the executor serves it.
#[derive(Clone, Deserialize)]
struct Signed {
    total: String,
    as_of: u64,
    signature: Bytes,
}

const DAY: u64 = 86_400;
const MINUTE: u64 = 60;
/// How long a closed vault stays in the round for its last bill, after the
/// later of its close and its halt's grace end: time for the agent's last
/// orders to land and the executor's meter to sign them.
const LAST_ORDERS: u64 = 15 * MINUTE;
/// The longest a halted vault's close, once it would go through, waits for
/// its bill, in chain time and on the keeper's own clock alike: a bill the
/// vault refuses for good (its key's blessing lapsed, its image revoked)
/// can't hold the vault open for good. As long as a closed vault's last bill
/// is waited for.
const MOST_CLOSE_HOLD: u64 = LAST_ORDERS;
const WAD: u64 = 1_000_000_000_000_000_000;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    let _ = OWN_CLOCK_HOLD.set(Duration::from_secs(args.own_clock_hold));
    let _ = SERVING_NONE_FOR.set(Duration::from_secs(args.serving_none_for));
    // Every chain to be kept is in the keeper's health from the start: one
    // that never connects is not kept, and is said to be.
    // A keeper asked for the Solana vault chain alone keeps no EVM chain.
    let evm = args.chain_args.vault_chain != Some(Chain::Solana);
    if evm {
        for target in args.chain_args.targets()? {
            chain_kept(target.vault_chain, |_| {});
        }
    }
    let solana_targets = args.chain_args.solana_targets()?;
    for target in &solana_targets {
        chain_kept(target.vault_chain, |_| {});
    }
    let mut connecting = if evm {
        args.chain_args.connect_each()?
    } else {
        tokio::sync::mpsc::unbounded_channel().1
    };
    // Each Solana vault chain is kept on its own, as it connects.
    let mut chains = tokio::task::JoinSet::new();
    if !solana_targets.is_empty() {
        let mut clusters = args.chain_args.connect_each_solana()?;
        let executor = args.executor_url.clone();
        chains.spawn(async move {
            let mut kept = tokio::task::JoinSet::new();
            while let Some(served) = clusters.recv().await {
                kept.spawn(solana::keep_chain(served, executor.clone()));
            }
            while kept.join_next().await.is_some() {}
        });
    }

    if args.port != 0 {
        tokio::spawn(serve_status(SocketAddr::from((args.listen, args.port))));
    }
    let attestation_http = reqwest::Client::new();
    // The chain whose registry records the root key: the executor's
    // `setRootKey` is relayed there alone.
    let root_chain = args.chain_args.root_chain()?;

    // Every vault chain turns on its own, from the moment it is connected,
    // so a slow chain, or one out of reach, holds up no other.
    while let Some(served) = connecting.recv().await {
        let chain = served.vault_chain;
        // A chain with no keeper account is not kept, and the others still
        // are.
        let keeper = match served.client.account(Role::Keeper) {
            Ok(keeper) => keeper,
            Err(error) => {
                tracing::warn!(chain = %served.key, error = format!("{error:#}"), "no keeper account on this chain; it is not kept");
                chain_kept(chain, |kept| kept.no_keeper = true);
                continue;
            }
        };
        let executor = args.executor_url.clone();
        let services: Vec<String> = args
            .service_attestation_urls
            .iter()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .collect();
        let attestation_http = attestation_http.clone();
        let canonical = root_chain
            .as_ref()
            .is_some_and(|root| served.key.same_chain(root));
        tracing::info!(chain = %served.key, %keeper, "the keeper is up");
        // Without a Closer a halted vault is closed step by step, and anyone
        // may get between its station's unwind and the valuation that finds
        // it empty.
        if served.deployments.closer == Address::ZERO {
            tracing::warn!(chain = %served.key, "this chain's deployment has no Closer; halted vaults are closed step by step");
        }
        chains.spawn(async move {
            // Nothing is sent that hasn't been simulated: a chain whose node
            // can't simulate is not kept until it can.
            until_it_can_simulate(chain, &served.client, &served.key).await;
            chain_kept(chain, |kept| {
                kept.cannot_simulate = false;
                kept.connected = Some(std::time::Instant::now());
            });
            // The executor's keys: their blessings relayed, and their gas
            // kept, on a cadence of their own, so a round slowed by the
            // vaults never holds up a fresh key's blessing, gas and approval.
            tokio::spawn(keep_keys(
                served.client.clone(),
                served.key.clone(),
                served.deployments.clone(),
                keeper,
                executor.clone(),
                attestation_http.clone(),
                canonical,
            ));
            // The builder's and the reviewer's keys, each on its own cadence.
            for service in &services {
                tokio::spawn(keep_service_keys(
                    served.client.clone(),
                    served.key.clone(),
                    served.deployments.clone(),
                    keeper,
                    service.clone(),
                    attestation_http.clone(),
                ));
            }
            let mut memory = HashMap::new();
            let mut factories = Factories::default();
            let mut said = Said::default();
            let mut closer = CloserAt::new(served.deployments.closer);
            let mut deployments = served.deployments.clone();
            loop {
                deployments.closer = closer.usable(&served.client, &served.key).await;
                let started = std::time::Instant::now();
                let done = tick(
                    chain,
                    &served.client,
                    &deployments,
                    keeper,
                    &executor,
                    &mut memory,
                    &mut factories,
                )
                .await;
                let now = std::time::Instant::now();
                match done {
                    Ok(()) => {
                        chain_kept(chain, |kept| {
                            kept.finished = Some(now);
                            kept.took = now.duration_since(started);
                            kept.failing = 0;
                        });
                        if said.over() {
                            tracing::info!(chain = %served.key, "the rounds finish again");
                        }
                    }
                    Err(error) => {
                        chain_kept(chain, |kept| kept.failing += 1);
                        if let Some(times) = said.due(now) {
                            tracing::warn!(chain = %served.key, times, error = format!("{error:#}"), "the round did not finish");
                        }
                    }
                }
                tokio::time::sleep(ROUND_PAUSE).await;
            }
        });
    }
    if chains.is_empty() && args.port == 0 {
        anyhow::bail!("no chain is kept: none has a keeper account");
    }
    while chains.join_next().await.is_some() {}
    // Nothing is kept; the health route still says why.
    std::future::pending::<()>().await;
    Ok(())
}

/// Relay the executor's blessings on one chain and keep its blessed keys in
/// gas, every [`attest::EVERY`], for as long as the keeper runs; on the
/// canonical chain, relay its root key's record too.
async fn keep_keys(
    client: Client,
    key: chain::ChainKey,
    deployments: Deployments,
    keeper: Address,
    executor: String,
    http: reqwest::Client,
    canonical: bool,
) {
    let mut blessings = attest::Memory::default();
    let mut said = Said::default();
    loop {
        let at = std::time::Instant::now();
        if blessings.due(at) {
            let looked = match attest::published(&http, &executor).await {
                Ok(published) => {
                    let registry = attest::OnChain {
                        client: &client,
                        deployments: &deployments,
                        keeper,
                    };
                    attest::look(
                        &registry,
                        client.chain_id,
                        canonical,
                        &published,
                        &mut blessings,
                        at,
                    )
                    .await
                }
                Err(error) => {
                    blessings.looked(at);
                    Err(error)
                }
            };
            match looked {
                Ok(()) => {
                    if said.over() {
                        tracing::info!(chain = %key, "the executor's keys are looked after again");
                    }
                }
                Err(error) => {
                    if let Some(times) = said.due(std::time::Instant::now()) {
                        tracing::warn!(chain = %key, times, error = format!("{error:#}"), "could not look after the executor's keys");
                    }
                }
            }
        }
        tokio::time::sleep(ROUND_PAUSE).await;
    }
}

/// Relay a builder's or a reviewer's blessings on one chain and keep its
/// blessed key in gas, every [`attest::EVERY`], for as long as the keeper
/// runs. The service is often off: that is said at debug only. Nothing it
/// publishes records the root key.
async fn keep_service_keys(
    client: Client,
    key: chain::ChainKey,
    deployments: Deployments,
    keeper: Address,
    service: String,
    http: reqwest::Client,
) {
    let mut blessings = attest::Memory::default();
    let mut said = Said::default();
    loop {
        let at = std::time::Instant::now();
        if blessings.due(at) {
            match attest::published(&http, &service).await {
                Ok(published) => {
                    let registry = attest::OnChain {
                        client: &client,
                        deployments: &deployments,
                        keeper,
                    };
                    let looked = attest::look(
                        &registry,
                        client.chain_id,
                        false,
                        &published,
                        &mut blessings,
                        at,
                    )
                    .await;
                    match looked {
                        Ok(()) => {
                            if said.over() {
                                tracing::info!(chain = %key, %service, "the service's key is looked after again");
                            }
                        }
                        Err(error) => {
                            if let Some(times) = said.due(std::time::Instant::now()) {
                                tracing::warn!(chain = %key, %service, times, error = format!("{error:#}"), "could not look after the service's key");
                            }
                        }
                    }
                }
                Err(error) => {
                    blessings.looked(at);
                    tracing::debug!(chain = %key, %service, error = format!("{error:#}"), "the service does not answer");
                }
            }
        }
        tokio::time::sleep(ROUND_PAUSE).await;
    }
}

/// How long the keeper waits between one round of a chain and the next.
const ROUND_PAUSE: Duration = Duration::from_secs(2);

/// How each vault chain to be kept is doing, for the keeper's health.
#[derive(Clone, Copy, Debug, Default)]
struct Kept {
    /// When it was connected, if it has been.
    connected: Option<std::time::Instant>,
    /// When its last round finished, and how long that round took.
    finished: Option<std::time::Instant>,
    took: Duration,
    /// How many of its rounds in a row have not finished.
    failing: u64,
    /// Whether it was connected and has no keeper account, so isn't kept.
    no_keeper: bool,
    /// Whether it was connected and its node can't simulate what an unwind
    /// leaves, so isn't kept until it can.
    cannot_simulate: bool,
    /// Whether an earlier factory's list of vaults, or the factory it
    /// replaces, couldn't be read in its last round: those vaults are kept as
    /// last listed, and any listed since are not.
    factories_unread: bool,
}

impl Kept {
    /// Rounds in a row that may fail before the chain is said not to be
    /// kept: one that fails now and then is a node's hiccup.
    const MOST_FAILING: u64 = 3;
    /// How long a chain may go without a round finishing, at least: a round
    /// that hangs fails no round, so it is judged by time as well.
    const STALLED_AFTER: Duration = Duration::from_secs(3 * 60);
    /// How many times as long as its last round took a chain may go without
    /// a round finishing: a chain with hundreds of vaults takes minutes a
    /// round.
    const STALLED_ROUNDS: u32 = 3;

    /// How long the chain may go without a round finishing before it is
    /// said not to be kept.
    fn stalled_after(&self) -> Duration {
        Self::STALLED_AFTER.max(self.took * Self::STALLED_ROUNDS)
    }

    /// Why the chain is not being kept, if it isn't.
    fn trouble(&self, now: std::time::Instant) -> Option<&'static str> {
        if self.no_keeper {
            return Some("no keeper account");
        }
        if self.cannot_simulate {
            return Some("can't simulate");
        }
        let Some(connected) = self.connected else {
            return Some("not connected");
        };
        let since = self.finished.unwrap_or(connected);
        if self.failing >= Self::MOST_FAILING || now.duration_since(since) > self.stalled_after() {
            return Some("rounds not finishing");
        }
        None
    }
}

/// Every vault chain to be kept, by its name.
static CHAINS: std::sync::LazyLock<std::sync::Mutex<std::collections::BTreeMap<Chain, Kept>>> =
    std::sync::LazyLock::new(Default::default);

/// Note something of a chain to be kept.
fn chain_kept(chain: Chain, change: impl FnOnce(&mut Kept)) {
    if let Ok(mut chains) = CHAINS.lock() {
        change(chains.entry(chain).or_default());
    }
}

/// The deployment's Closer, as a chain's rounds use it: only once something
/// is known to be deployed at its address. A call to an address with nothing
/// there goes through and does nothing, so a Closer named and not deployed
/// (a deployment file left from an earlier chain, say) would be paid for
/// every slot. Whether anything is there is read once per chain, again each
/// round until the read answers; the deployment is read once, when the chain
/// connects, so a new one means a restart and a fresh check.
struct CloserAt {
    at: Address,
    deployed: Option<bool>,
}

impl CloserAt {
    fn new(at: Address) -> Self {
        Self { at, deployed: None }
    }

    /// The Closer to use this round, or none (the zero address): halted
    /// vaults are then closed step by step.
    async fn usable(&mut self, client: &Client, chain: &impl std::fmt::Display) -> Address {
        if self.at == Address::ZERO {
            return Address::ZERO;
        }
        if self.deployed.is_none() {
            match client.has_code(self.at).await {
                Ok(deployed) => {
                    if !deployed {
                        tracing::warn!(%chain, closer = %self.at, "nothing is deployed at this chain's Closer address; halted vaults are closed step by step");
                    }
                    self.deployed = Some(deployed);
                }
                Err(error) => {
                    tracing::debug!(%chain, error = format!("{error:#}"), "could not read whether the Closer is deployed");
                }
            }
        }
        if self.deployed == Some(true) {
            self.at
        } else {
            Address::ZERO
        }
    }
}

/// A warning said at once, then at most once every [`Said::EVERY`] with how
/// many times it has held since.
#[derive(Default)]
struct Said {
    /// Times not yet said, and when it was last said.
    unsaid: u64,
    said_at: Option<std::time::Instant>,
}

impl Said {
    const EVERY: Duration = Duration::from_secs(10 * 60);

    /// Count one more time, and say how many to report if one is due.
    fn due(&mut self, now: std::time::Instant) -> Option<u64> {
        self.unsaid += 1;
        if self
            .said_at
            .is_some_and(|at| now.duration_since(at) < Self::EVERY)
        {
            return None;
        }
        self.said_at = Some(now);
        Some(std::mem::take(&mut self.unsaid))
    }

    /// The condition is over: its next time is said at once. Whether it had
    /// been said.
    fn over(&mut self) -> bool {
        self.unsaid = 0;
        self.said_at.take().is_some()
    }
}

/// What the keeper remembers about a vault between rounds, so it doesn't
/// read again what can no longer change, or send again what has been sent.
#[derive(Default)]
struct Memory {
    /// Claims before this one are paid or taken through the fallback: they
    /// never change again.
    claims_from: u64,
    /// Requests before this one are done, or are claims: the same.
    requests_from: u64,
    /// Who has taken units through the pro-rata fallback, and the block the
    /// logs were read up to.
    holders: BTreeSet<Address>,
    holders_read_to: u64,
    /// The chain time of the last daily round: the bill, the restricted list.
    daily_at: Option<u64>,
    /// The tank withdrawal the bill was last collected for.
    escape_collected: u64,
    /// The tank withdrawal last found asked for, and when the keeper first
    /// found it: only an answer to an ask sent since then can be the bill
    /// that withdrawal waits for.
    escape_seen: Option<(u64, std::time::Instant)>,
    /// When each waiting claim was last tried and may be tried next: one
    /// that can't be paid yet is tried again next slot, not every round, and
    /// less often the longer it keeps waiting.
    claims_tried: HashMap<u64, Tried>,
    /// The keeper's rounds in which the claims were looked at: a wait is
    /// counted in these as well as in slots, since a chain whose clock runs
    /// fast passes many slots a round.
    claim_rounds: u64,
    /// The claim the keeper last saw a close staged for: it goes to the back
    /// of the turn, so a claim whose close keeps ending with something left
    /// never keeps the others from theirs.
    last_staged: Option<u64>,
    /// The slot the stopped station was last asked to unwind in.
    unwound_in: Option<u64>,
    /// When the stopped station's unwind is next simulated, after
    /// simulations that found it not worth sending.
    unwind_checks: Option<Backoff>,
    /// The `asOf` of the last signed total a `keep` went through with, or
    /// that was counted settled while the vault refused it (see
    /// [`refused_bill_settled`]): each total is collected at most once. One
    /// mined and reverted, or refused before it went out, collected nothing
    /// and is tried again.
    bill_sent_as_of: u64,
    /// The `asOf` of the last signed total the vault refused a `keep`
    /// carrying.
    bill_refused_as_of: u64,
    /// The chain time the executor was last asked for a total outside the
    /// daily round.
    bill_asked_at: Option<u64>,
    /// When the vault's ask that got no answer, or a 404, is next made
    /// again (see [`ask_again`]).
    ask_again: Option<AskAgain>,
    /// The keeper's rounds this vault has been in, counting the one going on.
    rounds: u64,
    /// Rounds in a row in which the vault refused the `keep` carrying its
    /// bill, the round (of [`Memory::rounds`]) last counted, and their
    /// warning.
    bill_refused: u64,
    bill_refused_in: Option<u64>,
    bill_refused_said: Said,
    /// When a `keep` carrying the bill is next sent, after one that was
    /// mined and reverted (see [`send_again`]).
    keep_again: Option<SendAgain>,
    /// Whether the executor has been asked since the halt was seen, and
    /// since the halt's grace was first seen to be over.
    asked_at_halt: bool,
    asked_after_grace: bool,
    /// When the vault's close was first held for its bill, in chain time and
    /// on the keeper's clock (see [`collect_before_close`]). Cleared only
    /// when the bill settles, wherever in the round that happens (see
    /// [`bill_settled`]): a round that finds the close impossible, or a read
    /// that fails, leaves it as it is. A valuation the close needs is still
    /// recorded while it runs.
    close_held_since: Option<(u64, std::time::Instant)>,
    /// When the hold's last try was first made and didn't reach the vault, in
    /// chain time and on the keeper's clock: it is made again for at most as
    /// long as the hold (see [`collect_before_close`]). Cleared with the
    /// hold.
    last_try_since: Option<(u64, std::time::Instant)>,
    /// The end of the grace of the halt whose bill was written off when its
    /// hold ended: the close goes from then on, and no newer total holds it
    /// again for that halt.
    written_off_for: Option<u64>,
    /// Rounds in a row in which the vault's tank couldn't be read, and their
    /// warning.
    tank_unread: u64,
    tank_said: Said,
    /// When each holder's `serveProRata` is next sent, after one that was
    /// mined and reverted (see [`send_again`]).
    serve_again: HashMap<Address, SendAgain>,
    /// The first ask for a closed vault's total sent once its last bill's
    /// window was over: an answer to it or a later one settles the bill.
    /// Whether the last try (see [`last_bill`]) has settled it since: the
    /// window's end has one, and later totals wait as any other does.
    asked_past_window: Option<std::time::Instant>,
    tried_past_window: bool,
    /// When the vault was first seen closed, in chain time and on the
    /// keeper's clock: no earlier than its close, so the last bill's window
    /// measured from it is never cut short.
    closed_seen_at: Option<(u64, std::time::Instant)>,
    /// The chain time a closed or cancelled vault was last found with
    /// nothing left to do.
    quiet_since: Option<u64>,
    /// When the author retired the vault, once its log has been found, and
    /// the block that log was read up to until then.
    retired_at: Option<u64>,
    retired_read_to: u64,
    /// Keys a station's best-effort revoke left approved at the venue, by
    /// driver, not yet taken back; and the block its log was read up to.
    skipped: BTreeSet<(B256, Address)>,
    skipped_read_to: u64,
    /// The vault's rounds in a row that did not finish, and their warning.
    rounds_failing: u64,
    round_said: Said,
    /// The vault's state when a Closer call went through and left it open:
    /// the Closer is not sent again until the state changes.
    closer_closed_nothing: Option<u8>,
    /// Simulations in a row of what the stopped station's unwind leaves that
    /// failed, and their warning. Nothing is sent without one.
    probes_failing: u64,
    probe_said: Said,
    /// The vault's valuations, and its claims' tries, it refused before they
    /// went out, slot after slot.
    valuations_refused: Refusals,
    claims_refused: Refusals,
    /// The station's drivers whose venue acts after the transaction, and
    /// the pace of what is done for them (see [`delayed`]).
    delayed: delayed::Memory,
}

/// The slots in a row in which the vault refused a call of one kind before
/// it went out (its estimate reverted), and their warning. One refusal now
/// and then is ordinary (somebody else got there first, or it isn't time);
/// one that goes on for [`MOST_SLOTS_REFUSED`] is said at warn and in the
/// keeper's health, until a call of that kind goes through or the keeper no
/// longer wants one.
#[derive(Default)]
struct Refusals {
    slots: u64,
    last_slot: Option<u64>,
    said: Said,
}

/// Slots in a row a vault may refuse a valuation, or a claim's try, before
/// it is said.
const MOST_SLOTS_REFUSED: u64 = 10;

/// What the vault refuses, as the keeper's health counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Refused {
    Valuations,
    Claims,
}

impl Refused {
    fn what(self) -> &'static str {
        match self {
            Refused::Valuations => "valuations",
            Refused::Claims => "claims",
        }
    }
}

impl Refusals {
    /// Note what became of a call of this kind sent in `slot`.
    fn note(&mut self, outcome: Outcome, kind: Refused, chain_id: u64, vault: Address, slot: u64) {
        let key = (chain_id, vault, kind);
        match outcome {
            Outcome::Refused => {
                if self.last_slot != Some(slot) {
                    self.last_slot = Some(slot);
                    self.slots += 1;
                }
                if self.slots < MOST_SLOTS_REFUSED {
                    return;
                }
                if let Ok(mut refused) = CALLS_REFUSED.lock() {
                    refused.insert(key, self.slots);
                }
                if let Some(times) = self.said.due(std::time::Instant::now()) {
                    tracing::warn!(
                        chain_id,
                        %vault,
                        what = kind.what(),
                        slots = self.slots,
                        times,
                        "the vault keeps refusing these slot after slot"
                    );
                }
            }
            Outcome::Done => {
                if self.over(kind, chain_id, vault) {
                    tracing::info!(chain_id, %vault, what = kind.what(), "the vault takes these again");
                }
            }
            // Says nothing of whether the vault would take it.
            Outcome::Reverted | Outcome::NotSent | Outcome::Stale => {}
        }
    }

    /// The keeper no longer wants a call of this kind from the vault (it
    /// closed, its claims are paid, its close needs no other valuation):
    /// nothing is refused any more, and the count starts again.
    fn not_wanted(&mut self, kind: Refused, chain_id: u64, vault: Address) {
        if self.over(kind, chain_id, vault) {
            tracing::info!(chain_id, %vault, what = kind.what(), "these are no longer wanted of the vault");
        }
    }

    /// Start the count again, and whether it had been said.
    fn over(&mut self, kind: Refused, chain_id: u64, vault: Address) -> bool {
        self.slots = 0;
        self.last_slot = None;
        if let Ok(mut refused) = CALLS_REFUSED.lock() {
            refused.remove(&(chain_id, vault, kind));
        }
        self.said.over()
    }
}

/// When an ask that got no answer is made again, in chain time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AskAgain {
    /// How long it was told to wait.
    wait: u64,
    /// The earliest it is made.
    next: u64,
}

/// When a call is sent again after one that was mined and reverted: a `keep`
/// carrying the bill, or a holder's `serveProRata`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SendAgain {
    /// How many slots it was told to wait.
    wait: u64,
    /// The earliest it is sent, in chain time.
    next: u64,
    /// The first of the keeper's rounds (see [`Memory::rounds`]) it may be
    /// sent in: a chain whose clock runs fast (the devnet moves an hour a
    /// block) passes many slots in a round, and the wait is whichever of the
    /// two is longer.
    next_round: u64,
}

impl SendAgain {
    /// The keeper's round the call was mined and reverted in.
    fn reverted_in(&self) -> u64 {
        self.next_round.saturating_sub(self.wait)
    }
}

/// The longest wait, in chain time, between calls that are mined and revert
/// (see [`send_again`]).
const MOST_SECONDS_BETWEEN_REVERTS: u64 = 60 * MINUTE;

/// When a call is sent again after one, sent at `now` in the keeper's round
/// `round`, that was mined and reverted: next slot, then two slots on, four
/// and so on up to [`MOST_SECONDS_BETWEEN_REVERTS`], and no sooner than as
/// many of the keeper's rounds on. Each of those paid its gas and did
/// nothing, so they are not sent every round; one the vault refuses before it
/// goes out costs nothing and doesn't lengthen the wait.
///
/// A `keep`'s wait is the vault's, whatever total is sent: a newer total
/// waits it out too, so a stream of newer totals can't have every round pay
/// for a `keep` that reverts. Only a `keep` that goes through ends it; a try
/// made before the bill is counted settled doesn't wait for it (see
/// [`collect_answered`]). A holder's `serveProRata` waits on its own, until
/// a serve of that holder goes through (see [`units`]).
fn send_again(before: Option<&SendAgain>, now: u64, interval: u64, round: u64) -> SendAgain {
    let interval = interval.max(1);
    let most = (MOST_SECONDS_BETWEEN_REVERTS / interval).max(1);
    let wait = before
        .map(|before| (before.wait * 2).clamp(1, most))
        .unwrap_or(1);
    SendAgain {
        wait,
        next: now.saturating_add(wait * interval),
        next_round: round.saturating_add(wait),
    }
}

async fn tick(
    chain: Chain,
    client: &Client,
    deployments: &Deployments,
    keeper: Address,
    executor: &str,
    memory: &mut HashMap<Address, Memory>,
    factories: &mut Factories,
) -> Result<()> {
    let (vaults, unread_factories) = every_vault(client, deployments.factory, factories).await?;
    let factories_unread = !unread_factories.is_empty();
    chain_kept(chain, |kept| kept.factories_unread = factories_unread);
    if factories_unread {
        if let Some(times) = factories.unread_said.due(std::time::Instant::now()) {
            tracing::warn!(%chain, factories = ?unread_factories, times, "could not read an earlier factory's vaults, or the factory it replaces; its vaults are kept as last listed");
        }
    } else if factories.unread_said.over() {
        tracing::info!(%chain, "every factory's vaults are read again");
    }
    for &vault in &vaults {
        let remembered = memory.entry(vault).or_default();
        let done = keep_one(client, keeper, deployments, executor, vault, remembered).await;
        steps_worked(client.chain_id, vault, done.is_ok());
        let unkept = (client.chain_id, vault);
        match done {
            Ok(()) => {
                remembered.rounds_failing = 0;
                if remembered.round_said.over() {
                    tracing::info!(%chain, %vault, "the vault's rounds finish again");
                }
                if let Ok(mut unread) = UNREAD.lock() {
                    unread.remove(&unkept);
                }
            }
            Err(error) => {
                remembered.rounds_failing += 1;
                if let Some(times) = remembered.round_said.due(std::time::Instant::now()) {
                    tracing::warn!(%chain, %vault, times, error = format!("{error:#}"), "the vault's round did not finish");
                }
                if let Ok(mut unread) = UNREAD.lock() {
                    unread.insert(unkept, remembered.rounds_failing);
                }
            }
        }
        if let Ok(mut unprobed) = UNPROBED.lock() {
            if remembered.probes_failing >= MOST_PROBES_FAILING {
                unprobed.insert(unkept, remembered.probes_failing);
            } else {
                unprobed.remove(&unkept);
            }
        }
    }
    // A vault no longer listed is no longer said to be unkept.
    let listed =
        |chain_id: u64, vault: &Address| chain_id != client.chain_id || vaults.contains(vault);
    if let Ok(mut unread) = UNREAD.lock() {
        unread.retain(|(chain_id, vault), _| listed(*chain_id, vault));
    }
    if let Ok(mut unprobed) = UNPROBED.lock() {
        unprobed.retain(|(chain_id, vault), _| listed(*chain_id, vault));
    }
    if let Ok(mut unread) = TANKS_UNREAD.lock() {
        unread.retain(|(chain_id, vault), _| listed(*chain_id, vault));
    }
    if let Ok(mut refused) = BILLS_REFUSED.lock() {
        refused.retain(|(chain_id, vault), _| listed(*chain_id, vault));
    }
    if let Ok(mut failed) = STEPS_FAILED.lock() {
        failed.retain(|(chain_id, vault, _), _| listed(*chain_id, vault));
    }
    if let Ok(mut refused) = CALLS_REFUSED.lock() {
        refused.retain(|(chain_id, vault, _), _| listed(*chain_id, vault));
    }
    if let Ok(mut asks) = executor_for(executor).asks.lock() {
        asks.unanswered
            .retain(|(chain_id, vault)| listed(*chain_id, vault));
        asks.not_wanted
            .retain(|(chain_id, vault), _| listed(*chain_id, vault));
        asks.latest
            .retain(|(chain_id, vault), _| listed(*chain_id, vault));
        asks.note_serving();
    }
    say_serving_none(executor);
    // What the round no longer tries is no longer said to be failing.
    let now = client.block_timestamp().await?;
    if let Ok(mut not_sent) = NOT_SENT.lock() {
        not_sent.sweep(|(rpc, ..)| *rpc == client.rpc_url, now);
    }
    Ok(())
}

alloy::sol! {
    /// The factory a vault factory replaces, or none. Not in the factory's
    /// shared interface.
    interface IReplacesFactory {
        function previous() external view returns (address);
    }
}

/// The most factories followed back from the deployment's, so a chain of
/// them that loops can't hold the round.
const MOST_FACTORIES: usize = 16;

/// What a chain's factories listed when last read, so an earlier factory
/// whose list can't be read this round still has its vaults kept.
#[derive(Default)]
struct Factories {
    /// Each factory's vaults, and the factory it replaces, as last read.
    vaults: HashMap<Address, Vec<Address>>,
    previous: HashMap<Address, Address>,
    /// The rounds in which a list couldn't be read, as said.
    unread_said: Said,
}

/// Every vault to keep: those of the deployment's factory, and of every
/// factory it replaces, followed back through `previous` until there is
/// none. A replacement factory leaves the vaults made before it running, and
/// they are owed the same upkeep. Also the factories whose list of vaults,
/// or the factory they replace, couldn't be read this round.
///
/// The deployment's own factory must be read. An earlier one whose list, or
/// whose `previous`, can't be read this round (a node's `eth_call` gas cap on
/// a long list, say) is taken as it was last read, so its vaults stay in the
/// round and their alarms stand; one never read yet ends the walk there. A
/// factory whose `previous` reverts (one made before factories named the one
/// they replace) names none.
///
/// Their logs are read from the deployment's block: an earlier factory's own
/// deployment block isn't recorded. A vault listed by two factories is kept
/// once.
async fn every_vault(
    client: &Client,
    factory: Address,
    factories: &mut Factories,
) -> Result<(Vec<Address>, Vec<Address>)> {
    let mut vaults = Vec::new();
    let mut unread = Vec::new();
    let mut seen = Vec::new();
    let mut factory = factory;
    while factory != Address::ZERO && !seen.contains(&factory) && seen.len() < MOST_FACTORIES {
        seen.push(factory);
        let listed = match client.read(factory, IVaultFactory::vaultsCall {}).await {
            Ok(listed) => {
                factories.vaults.insert(factory, listed.clone());
                listed
            }
            Err(error) if seen.len() > 1 => {
                tracing::debug!(%factory, error = format!("{error:#}"), "an earlier factory's vaults not read; kept as last listed");
                unread.push(factory);
                factories.vaults.get(&factory).cloned().unwrap_or_default()
            }
            Err(error) => return Err(error),
        };
        for vault in listed {
            if !vaults.contains(&vault) {
                vaults.push(vault);
            }
        }
        factory = match client
            .read(factory, IReplacesFactory::previousCall {})
            .await
        {
            Ok(previous) => {
                factories.previous.insert(factory, previous);
                previous
            }
            Err(error) if execution_reverted(&error) => Address::ZERO,
            Err(error) => {
                tracing::debug!(%factory, error = format!("{error:#}"), "could not read the factory this one replaces");
                if !unread.contains(&factory) {
                    unread.push(factory);
                }
                factories
                    .previous
                    .get(&factory)
                    .copied()
                    .unwrap_or(Address::ZERO)
            }
        };
    }
    Ok((vaults, unread))
}

/// A step that failed is said at `warn` (at once, then at most every ten
/// minutes with a count, for each chain, vault and step) and the round
/// carries on: one that can't be read this round mustn't hold up the steps
/// after it. A round of the vault that finishes without it failing ends it
/// (see [`steps_worked`]): its next failure is said at once.
fn carry_on(chain_id: u64, vault: Address, step: &'static str, done: Result<()>) {
    if let Err(error) = done {
        let due = STEPS_FAILED.lock().ok().and_then(|mut failed| {
            let failed = failed.entry((chain_id, vault, step)).or_default();
            failed.this_round = true;
            failed.said.due(std::time::Instant::now())
        });
        if let Some(times) = due {
            tracing::warn!(chain_id, %vault, step, times, error = format!("{error:#}"), "a step did not go through");
        }
    }
}

/// The vault's round is over: if it `finished`, the steps that failed before
/// and not in it go through again. Either way the next round starts afresh.
fn steps_worked(chain_id: u64, vault: Address, finished: bool) {
    let Ok(mut failed) = STEPS_FAILED.lock() else {
        return;
    };
    failed.retain(|&(on, at, step), failed| {
        if on != chain_id || at != vault {
            return true;
        }
        if finished && !failed.this_round {
            if failed.said.over() {
                tracing::info!(chain_id, %vault, step, "the step goes through again");
            }
            return false;
        }
        failed.this_round = false;
        true
    });
}

/// A step that failed, as said (see [`carry_on`]).
#[derive(Default)]
struct StepFailed {
    said: Said,
    /// Whether it failed in the vault's round going on now.
    this_round: bool,
}

/// The steps that failed, by chain id, vault and step (see [`carry_on`]).
type StepsFailed = HashMap<(u64, Address, &'static str), StepFailed>;

static STEPS_FAILED: std::sync::LazyLock<std::sync::Mutex<StepsFailed>> =
    std::sync::LazyLock::new(Default::default);

/// Everything one vault might need this round. Each step is tried on its own:
/// one that can't go through yet doesn't hold up the others.
async fn keep_one(
    client: &Client,
    keeper: Address,
    deployments: &Deployments,
    executor: &str,
    vault: Address,
    memory: &mut Memory,
) -> Result<()> {
    memory.rounds += 1;
    let state = client.read(vault, IVault::stateCall {}).await?;
    let now = client.block_timestamp().await?;
    // Closed and cancelled vaults only ever accumulate: one found with
    // nothing left to do is looked at again once a day, not every round.
    if matches!(state, VaultState::Closed | VaultState::Cancelled)
        && memory
            .quiet_since
            .is_some_and(|at| now < at.saturating_add(DAY))
    {
        return Ok(());
    }
    let timers = client.read(vault, IVault::timersCall {}).await?;
    let from_block = deployments.deployed_block;

    if matches!(
        state,
        VaultState::Closed | VaultState::Cancelled | VaultState::None | VaultState::Activating
    ) {
        tank_read_again(client.chain_id, vault, memory);
    }
    match state {
        VaultState::Closed => {
            return closed_round(
                client, keeper, executor, vault, from_block, now, &timers, memory,
            )
            .await;
        }
        // A vault cancelled before it opened may still have a key approved
        // at its station.
        VaultState::Cancelled => {
            let station = client.read(vault, IVault::homeStationCall {}).await?;
            let keys_left = revoke_keys(client, keeper, station).await;
            let skipped_left =
                revoke_skipped(client, keeper, vault, station, from_block, memory).await;
            if !keys_left && !skipped_left {
                memory.quiet_since = Some(now);
            }
            return Ok(());
        }
        VaultState::None => return Ok(()),
        VaultState::Activating => {
            let created = client.read(vault, IVault::createdAtCall {}).await?;
            if now >= created + timers.activationWindow {
                send(client, keeper, vault, IVault::cancelCall {}, "cancel").await;
            }
            return Ok(());
        }
        _ => {}
    }

    // How long a transaction sent now takes to land. One that can't be
    // measured sends nothing that has to land inside a window this round.
    let block_interval = match block_interval(client).await {
        Ok(interval) => interval,
        Err(error) => {
            carry_on(
                client.chain_id,
                vault,
                "measure the block interval",
                Err(error),
            );
            u64::MAX
        }
    };

    // The bill, once a chain-day, with the total the executor signed, and
    // the restricted list read again on the same day. The day is the
    // keeper's own: anyone may collect the bill, and a collection somebody
    // else made mustn't put off the rest of the round.
    //
    // A read that fails holds up only the steps that need it: the bill and
    // the halt's steps wait for the next round, and the valuation, the
    // settlement and the claims go ahead.
    let tank = tank_read(
        client.chain_id,
        vault,
        client.read(vault, IVault::tankCall {}).await,
        memory,
    );
    let halted = match client.read(vault, IStationVault::haltedAtCall {}).await {
        Ok(halted) => Some(halted),
        Err(error) => {
            carry_on(
                client.chain_id,
                vault,
                "read whether the vault has halted",
                Err(error),
            );
            None
        }
    };
    let grace_end = match halted {
        Some(halted) => match grace_end(client, vault, halted).await {
            Ok(end) => end,
            Err(error) => {
                carry_on(client.chain_id, vault, "read the halt's grace", Err(error));
                None
            }
        },
        None => None,
    };
    let due = memory.daily_at.is_none_or(|at| now >= at + DAY);
    let mut refresh = false;
    if let (Some(tank), Some(halted)) = (&tank, halted) {
        let lane = if halted == 0 {
            Lane::Daily
        } else {
            Lane::Halted
        };
        if tank.escapeRequestedAt != 0
            && memory
                .escape_seen
                .is_none_or(|(asked, _)| asked != tank.escapeRequestedAt)
        {
            memory.escape_seen = Some((tank.escapeRequestedAt, std::time::Instant::now()));
        }
        // Whatever newer total the executor has answered with since, from
        // any ask, is collected, once.
        let (billing, answered) = collect_answered(
            client,
            keeper,
            executor,
            vault,
            tank,
            grace_end,
            now,
            timers.slotInterval,
            false,
            memory,
            "keep",
        )
        .await;
        // A withdrawal's bill is collected only with an answer to an ask
        // sent after it was asked for: an older answer leaves out what the
        // agent used since.
        let after_escape =
            answered
                .zip(memory.escape_seen)
                .is_some_and(|(answered, (asked, seen))| {
                    asked == tank.escapeRequestedAt && answered >= seen
                });
        if billing == Billing::Capped || (billing == Billing::Collected && after_escape) {
            memory.escape_collected = tank.escapeRequestedAt;
        }
        ask_again(client.chain_id, executor, vault, lane, now, memory);
        if due {
            memory.daily_at = Some(now);
            // The executor is asked once a day for an open vault's total, off
            // the round; its answer is collected in a round after. A halted
            // vault's is asked on the halt's own schedule.
            if halted == 0 && nothing_to_charge(tank, None).is_none() {
                ask(client.chain_id, executor, vault, Lane::Daily);
            }
            // The fill and the restricted list are the day's, done once.
            let fill_wanted = halted == 0
                && tank.keepAt > tank.balance
                && !tank.fillPending
                && match can_fill(client, vault).await {
                    Ok(can) => can,
                    Err(error) => {
                        carry_on(
                            client.chain_id,
                            vault,
                            "read what the tank could be filled from",
                            Err(error),
                        );
                        false
                    }
                };
            let daily = daily(matches!(state, VaultState::Open), halted != 0, fill_wanted);
            // A `keep` that collected a total this round queued the fill too.
            if daily.keep && billing != Billing::Collected {
                send(client, keeper, vault, keep_call(None), "keep").await;
            }
            refresh = daily.refresh;
        }
        if let Some(grace_end) = grace_end {
            ask_halted(
                client.chain_id,
                executor,
                vault,
                tank,
                now,
                grace_end,
                memory,
            );
        }
        ask_for_escape(
            client.chain_id,
            executor,
            vault,
            lane,
            tank,
            grace_end,
            now,
            memory,
        );
    }

    let station = match client.read(vault, IVault::homeStationCall {}).await {
        Ok(station) => Some(station),
        Err(error) => {
            carry_on(
                client.chain_id,
                vault,
                "read the vault's home station",
                Err(error),
            );
            None
        }
    };
    if let Some(station) = station {
        revoke_keys(client, keeper, station).await;
        revoke_skipped(client, keeper, vault, station, from_block, memory).await;
        carry_on(
            client.chain_id,
            vault,
            "move the delayed venue's steps on",
            delayed_steps(client, keeper, station, memory).await,
        );
    }

    // A halted vault's station is stopped and emptied before the slot's
    // valuation, so that valuation finds it empty and the vault can close in
    // the same round.
    let mut closed = false;
    if let Some(grace_end) = grace_end
        && let Some(station) = station
        && now >= grace_end
    {
        match stop_and_unwind(
            client,
            keeper,
            executor,
            deployments.closer,
            vault,
            station,
            &timers,
            grace_end,
            block_interval,
            memory,
        )
        .await
        {
            Ok(done) => closed = done,
            Err(error) => carry_on(
                client.chain_id,
                vault,
                "stop and unwind the station",
                Err(error),
            ),
        }
    }

    // A close held for its bill already has the valuation it needs: none is
    // paid for meanwhile, unless the close would no longer go through.
    let held = memory.close_held_since.is_some()
        && grace_end.is_some()
        && client.read(vault, IVault::closeCall {}).await.is_ok();
    let halt = match (halted, grace_end) {
        (Some(0), _) => Halt::Running,
        (Some(_), Some(_)) => Halt::Halted,
        _ => Halt::Unread,
    };
    if !closed {
        price(
            client,
            keeper,
            vault,
            &timers,
            block_interval,
            !held,
            halt,
            memory,
        )
        .await;
    }
    // After the round's settle, so the list is read against what was priced.
    if refresh {
        send(
            client,
            keeper,
            vault,
            IVault::refreshRestrictedCall {},
            "refresh the restricted list",
        )
        .await;
    }
    // A halt that couldn't be read is taken as none here: what a halt rules
    // out (a freeze) the vault refuses itself.
    let halted_or_not = halted.unwrap_or(0);
    carry_on(
        client.chain_id,
        vault,
        "claims",
        claims(client, keeper, vault, &timers, halted_or_not, memory).await,
    );
    carry_on(
        client.chain_id,
        vault,
        "units",
        units(
            client,
            keeper,
            vault,
            from_block,
            now,
            timers.freezeAfter,
            timers.slotInterval,
            halted_or_not,
            memory,
        )
        .await
        .map(|_| ()),
    );

    if matches!(state, VaultState::WindingDown) && halted == Some(0) {
        match retired_at(client, vault, from_block, memory).await {
            Ok(Some(at)) if now >= at + timers.windDown => {
                send(client, keeper, vault, IVault::evictCall {}, "evict").await;
            }
            Ok(_) => {}
            Err(error) => carry_on(
                client.chain_id,
                vault,
                "read when the vault was retired",
                Err(error),
            ),
        }
    }

    // `close` has no window: it goes through whenever a valuation after the
    // halt has found the stations empty and everything it priced is settled,
    // inside the grace or after it. Whether it would is asked of the vault
    // first, and the bill collected before it is sent: inside the grace that
    // collection is only made when the close will follow. A bill that didn't
    // go through holds the close for a later round.
    if let Some(grace_end) = grace_end
        && !closed
        && client.read(vault, IVault::closeCall {}).await.is_ok()
        && collect_before_close(
            client,
            keeper,
            executor,
            vault,
            grace_end,
            now,
            timers.slotInterval,
            true,
            memory,
        )
        .await
    {
        send(client, keeper, vault, IVault::closeCall {}, "close").await;
    }
    Ok(())
}

/// What a station with a delayed driver needs each round: its staged close
/// moved on, and money its account put into the venue's own vaults
/// reclaimed (see [`delayed`]). A station without one needs nothing here.
async fn delayed_steps(
    client: &Client,
    keeper: Address,
    station: Address,
    memory: &mut Memory,
) -> Result<()> {
    let drivers = delayed::drivers_of(client, station, &mut memory.delayed).await?;
    if drivers.is_empty() {
        return Ok(());
    }
    // A venue whose withdrawals wait to be paid out (Lighter's) has them
    // paid out to the station whenever any is pending, the executor's own
    // withdrawals as well as a staged close's, so the close can be delivered
    // in the same round.
    delayed::collect_withdrawals(client, keeper, station, &drivers).await?;
    delayed::settle_close(client, keeper, station).await?;
    delayed::reclaim(
        client,
        &VENUE_HTTP,
        keeper,
        station,
        &drivers,
        &mut memory.delayed,
    )
    .await
}

/// The client the keeper asks venues' APIs with.
static VENUE_HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("an HTTP client with a timeout")
});

/// The round's read of an open or halted vault's tank, noted: one that fails
/// [`MOST_VAULT_ROUNDS_FAILING`] rounds in a row is in the keeper's health
/// and said at warn (at once, then at most every ten minutes with a count)
/// until a read works, since without it no bill is collected and a halted
/// vault's close waits out its hold.
fn tank_read(
    chain_id: u64,
    vault: Address,
    read: Result<Tank>,
    memory: &mut Memory,
) -> Option<Tank> {
    match read {
        Ok(tank) => {
            tank_read_again(chain_id, vault, memory);
            Some(tank)
        }
        Err(error) => {
            memory.tank_unread += 1;
            if memory.tank_unread < MOST_VAULT_ROUNDS_FAILING {
                tracing::debug!(%vault, error = format!("{error:#}"), "could not read the tank");
                return None;
            }
            if let Ok(mut unread) = TANKS_UNREAD.lock() {
                unread.insert((chain_id, vault), memory.tank_unread);
            }
            if let Some(times) = memory.tank_said.due(std::time::Instant::now()) {
                tracing::warn!(
                    chain_id,
                    %vault,
                    rounds = memory.tank_unread,
                    times,
                    error = format!("{error:#}"),
                    "could not read the vault's tank round after round; its bill is not collected"
                );
            }
            None
        }
    }
}

/// The vault's tank was read, or is no longer read in the round (the vault
/// closed, say): it is no longer said to be unreadable.
fn tank_read_again(chain_id: u64, vault: Address, memory: &mut Memory) {
    memory.tank_unread = 0;
    if let Ok(mut unread) = TANKS_UNREAD.lock() {
        unread.remove(&(chain_id, vault));
    }
    if memory.tank_said.over() {
        tracing::info!(chain_id, %vault, "the vault's tank is no longer said to be unreadable");
    }
}

/// A closed vault: its station stopped and its keys taken back, its last
/// bill collected, and what was waiting when it closed paid out. One found
/// with nothing left to do is left alone for a day.
#[allow(clippy::too_many_arguments)]
async fn closed_round(
    client: &Client,
    keeper: Address,
    executor: &str,
    vault: Address,
    from_block: u64,
    now: u64,
    timers: &chain::bindings::vault::Timers,
    memory: &mut Memory,
) -> Result<()> {
    // A key's approval outlives the vault's closing until somebody takes it
    // back, and the station runs on until it is stopped.
    let mut keys_left = true;
    let mut station_left = true;
    if let Ok(station) = client.read(vault, IVault::homeStationCall {}).await {
        carry_on(
            client.chain_id,
            vault,
            "stop the closed vault's station",
            stop_after_grace(client, keeper, vault, station, now).await,
        );
        keys_left = revoke_keys(client, keeper, station).await;
        keys_left |= revoke_skipped(client, keeper, vault, station, from_block, memory).await;
        station_left = closed_station(client, keeper, vault, station, timers, now, memory)
            .await
            .unwrap_or_else(|error| {
                carry_on(
                    client.chain_id,
                    vault,
                    "empty the closed vault's station",
                    Err(error),
                );
                true
            });
    }
    // Nothing more is valued or tried as a claim: a closed vault pays its
    // claims itself.
    memory
        .valuations_refused
        .not_wanted(Refused::Valuations, client.chain_id, vault);
    memory
        .claims_refused
        .not_wanted(Refused::Claims, client.chain_id, vault);
    let closed_at = *memory
        .closed_seen_at
        .get_or_insert((now, std::time::Instant::now()));
    let billed = last_bill(
        client,
        keeper,
        executor,
        vault,
        closed_at,
        now,
        timers.slotInterval,
        memory,
    )
    .await
    .unwrap_or_else(|error| {
        carry_on(client.chain_id, vault, "collect the last bill", Err(error));
        false
    });
    // A closed vault is past freezing; its units are still served.
    let served = units(
        client,
        keeper,
        vault,
        from_block,
        now,
        timers.freezeAfter,
        timers.slotInterval,
        u64::MAX,
        memory,
    )
    .await
    .unwrap_or_else(|error| {
        carry_on(client.chain_id, vault, "units", Err(error));
        false
    });
    let paid = paid_out(client, keeper, vault, memory)
        .await
        .unwrap_or_else(|error| {
            carry_on(
                client.chain_id,
                vault,
                "pay out the closed vault",
                Err(error),
            );
            false
        });
    if !keys_left && !station_left && billed && served && paid {
        memory.quiet_since = Some(now);
    }
    Ok(())
}

/// A closed vault's station still has to be stopped once its grace is over,
/// and whatever it holds sent home, where the closed vault pays it to the
/// holders still to redeem: a vault closes with dust left at its station,
/// and anything that reaches the station later stays there unless it is
/// asked for. It is asked as a halted vault's is. Whether anything is left
/// to wait for: the station still running, or still worth something (or not
/// known to be worth nothing) that an unwind might yet move. A station whose
/// unwind was last simulated as not worth sending, and not blocked, is not
/// waited for: the vault may go quiet, and is looked at again, and its
/// station asked again, once a day. One whose unwind a market outside the
/// band blocks is waited for, at the simulations' pace: the market may come
/// back. So is one whose simulation failed, which is made again next slot
/// and said once it keeps failing (see [`MOST_PROBES_FAILING`]).
async fn closed_station(
    client: &Client,
    keeper: Address,
    vault: Address,
    station: Address,
    timers: &chain::bindings::vault::Timers,
    now: u64,
    memory: &mut Memory,
) -> Result<bool> {
    if !client.read(station, IStation::stoppedCall {}).await? {
        return Ok(true);
    }
    if let Ok(value) = client.read(station, IStation::valueCall {}).await
        && value.mark.is_zero()
    {
        probes_not_failing(vault, memory);
        return Ok(false);
    }
    unwind_stopped(client, keeper, vault, station, timers, now, memory).await?;
    Ok(!memory.unwind_checks.is_some_and(|checks| checks.idle))
}

/// A vault can close inside its halt's grace, before its station may be
/// stopped, and a running station's key can't be taken back while its
/// blessing stands: the station is stopped once the grace is over.
async fn stop_after_grace(
    client: &Client,
    keeper: Address,
    vault: Address,
    station: Address,
    now: u64,
) -> Result<()> {
    if client.read(station, IStation::stoppedCall {}).await? {
        return Ok(());
    }
    let halted = client.read(vault, IStationVault::haltedAtCall {}).await?;
    if grace_is_over(client, vault, now, halted).await? {
        send(
            client,
            keeper,
            station,
            IStation::stop_0Call {},
            "stop the closed vault's station",
        )
        .await;
    }
    Ok(())
}

/// What the daily round sends.
#[derive(Debug, PartialEq)]
struct Daily {
    /// A `keep` without a total, to queue a fill.
    keep: bool,
    refresh: bool,
}

/// The daily round for a vault, besides its bill, which is collected
/// whenever the executor has answered with a newer total (see
/// [`collect_answered`]). A `keep` without a total can only queue a fill of
/// the tank from the author's shares, so it is sent only when the author
/// keeps the tank above what it holds, no fill is already queued, there is
/// something to fill it from, and the vault hasn't halted (a halted vault is
/// stopping, and its tank needs no more). The restricted list keeps
/// newcomers out, so it is read again only while the vault is open to them.
fn daily(open: bool, halted: bool, fill_wanted: bool) -> Daily {
    Daily {
        keep: !halted && fill_wanted,
        refresh: open && !halted,
    }
}

/// Whether a signed total bills anything: more than the vault has collected,
/// signed later than the total it collected (the vault refuses one that
/// isn't), and later than the last total sent.
fn owes(signed: &Signed, tank: &Tank, sent_as_of: u64) -> bool {
    signed
        .total
        .parse::<U256>()
        .is_ok_and(|total| total > tank.usageSettled)
        && signed.as_of > tank.usageAsOf
        && signed.as_of > sent_as_of
}

/// Whether a fill could be paid: it redeems the author's shares from the
/// vault's cash, so the author must hold some and the vault must hold cash.
async fn can_fill(client: &Client, vault: Address) -> Result<bool> {
    let author = client.read(vault, IVault::authorCall {}).await?;
    let shares = client
        .read(vault, IVault::balanceOfCall { owner: author })
        .await?;
    if shares.is_zero() {
        return Ok(false);
    }
    let cash = client.read(vault, IVault::cashCall {}).await?;
    Ok(!cash.is_zero())
}

/// What became of collecting the bill with the executor's latest answer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Billing {
    /// The executor hasn't answered for the vault yet.
    NoAnswer,
    /// The executor answered that it serves no total for the vault.
    NotServed,
    /// The executor's latest total bills nothing the vault hasn't collected,
    /// or has been sent already.
    UpToDate,
    /// A `keep` with the latest total went through.
    Collected,
    /// The vault refused a `keep` with the latest total (its estimate or
    /// its transaction reverted): a key no longer blessed, say.
    Refused,
    /// A `keep` was sent and didn't reach the vault (no answer came back,
    /// or the node turned it away), or the vault refused it as `Stale`,
    /// which a node behind the chain also says: it is tried again.
    NotCollected,
    /// The tank is empty: a `keep` would charge nothing and write the whole
    /// bill off, so a later top-up could never pay it.
    EmptyTank,
    /// A collection has reached the halt's grace end, after which the
    /// allowances earn no more, and left no cap carried: the vault charges
    /// nothing more.
    Capped,
}

impl Billing {
    /// Whether nothing is left to collect with the latest answer in hand:
    /// its `keep` went through, or there was none to send. What a `keep`
    /// the vault or the node refused leaves owed is not.
    fn settled(self) -> bool {
        !matches!(self, Billing::Refused | Billing::NotCollected)
    }
}

/// Whether a `keep` with a signed total could only charge nothing, and why:
/// the tank is empty, or a collection has already reached the end of the
/// halt's grace, where the vault stops counting the allowances, and left no
/// cap carried. Either way it would only write the newer total off. Past the
/// grace a vault still charges a newer total out of the cap it carries (the
/// gas of the agent's last orders, landing late), so that is collected.
fn nothing_to_charge(tank: &Tank, grace_end: Option<u64>) -> Option<Billing> {
    if tank.balance.is_zero() {
        Some(Billing::EmptyTank)
    } else if grace_end.is_some_and(|end| tank.lastCollection >= end) && tank.capCarried.is_zero() {
        Some(Billing::Capped)
    } else {
        None
    }
}

/// Collect the bill with the latest answer the executor has given for the
/// vault, when it bills anything the vault could charge (see
/// [`nothing_to_charge`]; a halted vault's `grace_end` is given). The
/// executor isn't asked here (see [`ask`]): whatever answer has come back is
/// what is collected. Each total goes through in one `keep` at most; one the
/// vault refused (its estimate reverted, or it was mined and reverted) is
/// tried again, as is one the node turned away for its own reasons or the
/// vault refused as `Stale` (see [`Outcome::Stale`]), which isn't counted as
/// refused: one
/// refused before it went out every round, one mined and reverted after a
/// wait (see [`send_again`]), held as refused meanwhile. With `last_try`,
/// before a bill the vault refuses is counted settled, that wait is not
/// waited out: the latest total is tried once more, unless a `keep` carrying
/// it was mined and reverted earlier in the same round. Also, when the ask
/// the answer is for was sent.
///
/// A vault that refuses the `keep` is said at warn (at
/// once, then at most every ten minutes with a count) and is in the keeper's
/// health until its bill no longer waits on it: a `keep` goes through, there
/// is nothing newer to collect, or the vault can charge nothing more. The
/// refusals are counted once a keeper round (see [`Memory::rounds`]),
/// however often it tries. Any of those but a refusal settles the bill (see
/// [`bill_settled`]), which ends a close's hold for it.
#[allow(clippy::too_many_arguments)]
async fn collect_answered(
    client: &Client,
    keeper: Address,
    executor: &str,
    vault: Address,
    tank: &Tank,
    grace_end: Option<u64>,
    now: u64,
    slot_interval: u64,
    last_try: bool,
    memory: &mut Memory,
    what: &str,
) -> (Billing, Option<std::time::Instant>) {
    let (billing, asked) = collect_latest(
        client,
        keeper,
        executor,
        vault,
        tank,
        grace_end,
        now,
        slot_interval,
        last_try,
        memory,
        what,
    )
    .await;
    let key = (client.chain_id, vault);
    match billing {
        // Says nothing either way.
        Billing::NoAnswer | Billing::NotCollected => {}
        Billing::Refused => {
            if memory.bill_refused_in == Some(memory.rounds) {
                return (billing, asked);
            }
            memory.bill_refused_in = Some(memory.rounds);
            memory.bill_refused += 1;
            if let Ok(mut refused) = BILLS_REFUSED.lock() {
                refused.insert(key, memory.bill_refused);
            }
            if let Some(times) = memory.bill_refused_said.due(std::time::Instant::now()) {
                tracing::warn!(
                    chain_id = client.chain_id,
                    %vault,
                    times,
                    "the vault refuses the keep carrying its bill; it is tried again"
                );
            }
        }
        _ => {
            if bill_settled(client.chain_id, vault, memory) {
                tracing::info!(chain_id = client.chain_id, %vault, "the vault's bill no longer waits on a refused keep");
            }
        }
    }
    (billing, asked)
}

/// [`collect_answered`], before what became of it is noted.
#[allow(clippy::too_many_arguments)]
async fn collect_latest(
    client: &Client,
    keeper: Address,
    executor: &str,
    vault: Address,
    tank: &Tank,
    grace_end: Option<u64>,
    now: u64,
    slot_interval: u64,
    last_try: bool,
    memory: &mut Memory,
    what: &str,
) -> (Billing, Option<std::time::Instant>) {
    if let Some(nothing) = nothing_to_charge(tank, grace_end) {
        total_not_wanted(executor, client.chain_id, vault);
        return (nothing, None);
    }
    let Some((answer, asked)) = latest_answer(executor, client.chain_id, vault) else {
        return (Billing::NoAnswer, None);
    };
    let signed = match answer {
        Asked::NotServed => return (Billing::NotServed, Some(asked)),
        Asked::Total(signed) => signed,
    };
    if !owes(&signed, tank, memory.bill_sent_as_of) {
        return (Billing::UpToDate, Some(asked));
    }
    let as_of = signed.as_of;
    // The vault's last `keep` was mined and reverted: none is sent again,
    // whatever its total, until the wait is over, and the bill is still
    // refused meanwhile. A last try doesn't wait, unless this very total was
    // mined and reverted this round: it has been tried.
    if memory.keep_again.is_some_and(|again| {
        (now < again.next || memory.rounds < again.next_round)
            && (!last_try
                || (again.reverted_in() == memory.rounds && memory.bill_refused_as_of == as_of))
    }) {
        return (Billing::Refused, Some(asked));
    }
    let billing = match send_outcome(client, keeper, vault, keep_call(Some(signed)), what).await {
        Outcome::Done => Billing::Collected,
        // Mined and reverted collected nothing, as much as refused before it
        // went out: the total is tried again, after a wait, since it paid
        // for its gas.
        Outcome::Reverted => {
            memory.keep_again = Some(send_again(
                memory.keep_again.as_ref(),
                now,
                slot_interval,
                memory.rounds,
            ));
            memory.bill_refused_as_of = as_of;
            return (Billing::Refused, Some(asked));
        }
        Outcome::Refused => {
            memory.bill_refused_as_of = as_of;
            return (Billing::Refused, Some(asked));
        }
        // The keeper's node may be behind the chain: not the vault refusing
        // the total.
        Outcome::NotSent | Outcome::Stale => return (Billing::NotCollected, Some(asked)),
    };
    memory.keep_again = None;
    memory.bill_sent_as_of = as_of;
    (billing, Some(asked))
}

/// A closed vault's last bill, and whether it is settled.
///
/// The executor is asked once a minute, and each newer total it answers with
/// is collected (see [`collect_answered`]). Until [`LAST_ORDERS`] after the
/// later of the close and the halt's grace end the bill is never settled:
/// the agent's last orders may still be landing, and the author of a closed
/// vault may take the tank at once. After that it is settled once an answer
/// to an ask sent since says there is nothing more to collect (the executor
/// serves no total for the vault, its latest total is already collected, or
/// the vault refuses it), or once the vault could charge nothing (see
/// [`nothing_to_charge`]). The answer that settles it is tried first, once
/// more, whatever wait a reverted `keep` left (see [`send_again`]); a try that
/// doesn't reach the vault is made again, and once the bill is settled, later
/// totals wait as any other does. No answer
/// says nothing either way, so the vault stays in the round. A settled vault
/// is looked at again a day later. The window is measured in chain time and
/// on the keeper's clock alike (see [`waited`]).
#[allow(clippy::too_many_arguments)]
async fn last_bill(
    client: &Client,
    keeper: Address,
    executor: &str,
    vault: Address,
    closed_at: (u64, std::time::Instant),
    now: u64,
    slot_interval: u64,
    memory: &mut Memory,
) -> Result<bool> {
    let tank = client.read(vault, IVault::tankCall {}).await?;
    let halted = client.read(vault, IStationVault::haltedAtCall {}).await?;
    let grace_end = grace_end(client, vault, halted).await?;
    let (seen, seen_on_our_clock) = closed_at;
    let over = waited((seen.max(grace_end.unwrap_or(0)), seen_on_our_clock), now);
    // An answer to an ask sent since the window ended is the last word, and
    // is tried once more until it settles the bill.
    let past_window = over
        && latest_answer(executor, client.chain_id, vault)
            .zip(memory.asked_past_window)
            .is_some_and(|((_, answered), past)| answered >= past);
    let last_try = past_window && !memory.tried_past_window;
    let (billing, answered) = collect_answered(
        client,
        keeper,
        executor,
        vault,
        &tank,
        grace_end,
        now,
        slot_interval,
        last_try,
        memory,
        "collect the last bill",
    )
    .await;
    if matches!(billing, Billing::EmptyTank | Billing::Capped) {
        return Ok(over);
    }
    if memory
        .bill_asked_at
        .is_none_or(|at| now >= at.saturating_add(MINUTE))
        && let Some(sent) = ask(client.chain_id, executor, vault, Lane::Halted)
    {
        memory.bill_asked_at = Some(now);
        if over && memory.asked_past_window.is_none() {
            memory.asked_past_window = Some(sent);
        }
    }
    let settled = past_window
        && answered
            .zip(memory.asked_past_window)
            .is_some_and(|(answered, past)| answered >= past)
        && matches!(
            billing,
            Billing::NotServed | Billing::UpToDate | Billing::Collected | Billing::Refused
        );
    if settled {
        memory.tried_past_window = true;
        total_not_wanted(executor, client.chain_id, vault);
        refused_bill_settled(client.chain_id, vault, memory);
    }
    Ok(settled)
}

/// The `keep` call for a signed total, or without one.
fn keep_call(signed: Option<Signed>) -> IVault::keepCall {
    match signed {
        Some(signed) => IVault::keepCall {
            total: signed.total.parse().unwrap_or_default(),
            asOf: signed.as_of,
            signature: signed.signature,
        },
        None => IVault::keepCall {
            total: U256::ZERO,
            asOf: 0,
            signature: Bytes::new(),
        },
    }
}

/// The author has asked to withdraw the tank without a signed total: the
/// executor is asked for its latest at most once a minute, until a `keep`
/// with a total it answered with goes through or the vault can charge
/// nothing more, so the withdrawal can't take what the agent has already
/// used. A halted vault's too, whose grace may well outlast the escape's
/// delay.
#[allow(clippy::too_many_arguments)]
fn ask_for_escape(
    chain_id: u64,
    executor: &str,
    vault: Address,
    lane: Lane,
    tank: &Tank,
    grace_end: Option<u64>,
    now: u64,
    memory: &mut Memory,
) {
    let asked = tank.escapeRequestedAt;
    if asked == 0
        || asked == memory.escape_collected
        || nothing_to_charge(tank, grace_end).is_some()
        || memory
            .bill_asked_at
            .is_some_and(|at| now < at.saturating_add(MINUTE))
    {
        return;
    }
    if ask(chain_id, executor, vault, lane).is_some() {
        memory.bill_asked_at = Some(now);
    }
}

/// Take back the venue approval of a key whose blessing has gone, or whose
/// driver the registry revoked. The station refuses while the key still
/// stands, so a refusal here only means there is nothing to do. Whether any
/// key may still be approved afterwards.
async fn revoke_keys(client: &Client, keeper: Address, station: Address) -> bool {
    let Ok(spec) = client.read(station, IStation::specCall {}).await else {
        return true;
    };
    let mut left = false;
    for driver in spec.drivers {
        let Ok(key) = client
            .read(station, IStation::approvedKeyCall { driver })
            .await
        else {
            left = true;
            continue;
        };
        if key == Address::ZERO {
            continue;
        }
        left |= !send(
            client,
            keeper,
            station,
            IStation::revokeKeyCall { driver, key },
            "revoke a key that no longer stands",
        )
        .await;
    }
    left
}

/// Take back the venue approval of a key a station's best-effort revoke
/// left in place: a handover from a key whose blessing lapsed, or a stop,
/// says so with `RevokeSkipped`, and anyone may finish it strictly with
/// `revokeKey`. The station refuses that while the key still stands, when its
/// gas is estimated and at no cost, so each is sent every round until it
/// goes through. One the log shows revoked since is not sent. The log is read
/// on from where the last read stopped. Whether any may be left: one not yet
/// taken back, or a log not read up to the head.
async fn revoke_skipped(
    client: &Client,
    keeper: Address,
    vault: Address,
    station: Address,
    from_block: u64,
    memory: &mut Memory,
) -> bool {
    // The keys already known are taken back even if the log can't be read
    // this round.
    let read_to_head = match find_skipped(client, station, from_block, memory).await {
        Ok(caught_up) => caught_up,
        Err(error) => {
            carry_on(
                client.chain_id,
                vault,
                "read which revokes were skipped",
                Err(error),
            );
            false
        }
    };
    let mut taken_back = Vec::new();
    for &(driver, key) in &memory.skipped {
        if send(
            client,
            keeper,
            station,
            IStation::revokeKeyCall { driver, key },
            "revoke a key a skipped revoke left approved",
        )
        .await
        {
            taken_back.push((driver, key));
        }
    }
    for done in taken_back {
        memory.skipped.remove(&done);
    }
    !read_to_head || !memory.skipped.is_empty()
}

/// Read on through a station's log for the revokes it skipped, and the
/// revokes that went through since. Whether it has been read up to the head.
async fn find_skipped(
    client: &Client,
    station: Address,
    from_block: u64,
    memory: &mut Memory,
) -> Result<bool> {
    use alloy::sol_types::SolEvent;
    let head = client.block_number().await?;
    let filter = alloy::rpc::types::Filter::new()
        .address(station)
        .event_signature(vec![
            IStation::RevokeSkipped::SIGNATURE_HASH,
            IStation::KeyRevoked::SIGNATURE_HASH,
        ]);
    for _ in 0..LOG_CHUNKS_PER_ROUND {
        let start = memory.skipped_read_to.max(from_block);
        if start > head {
            break;
        }
        let (logs, end) = logs_from(
            client,
            &filter,
            start,
            start.saturating_add(LOG_CHUNK - 1).min(head),
        )
        .await?;
        for log in logs {
            if let Ok(skipped) = log.log_decode::<IStation::RevokeSkipped>() {
                let skipped = skipped.inner.data;
                memory.skipped.insert((skipped.driver, skipped.key));
            } else if let Ok(revoked) = log.log_decode::<IStation::KeyRevoked>() {
                let revoked = revoked.inner.data;
                memory.skipped.remove(&(revoked.driver, revoked.key));
            }
        }
        memory.skipped_read_to = end + 1;
    }
    Ok(memory.skipped_read_to.max(from_block) > head)
}

/// The slot a moment falls in.
fn slot_of(now: u64, interval: u64) -> u64 {
    now - now % interval.max(1)
}

/// How far apart the chain's blocks are, from the last two: a transaction
/// sent now lands in the next block, about this long after the head. At
/// least a second, since a chain's clock counts whole seconds and the next
/// block may fall in the next one.
async fn block_interval(client: &Client) -> Result<u64> {
    let head = client.block_number().await?;
    let latest = client.block_timestamp_at(head).await?;
    let before = client.block_timestamp_at(head.saturating_sub(1)).await?;
    Ok(latest.saturating_sub(before).max(1))
}

/// The slot a transaction sent with the head at `now` is judged in, if it
/// lands inside that slot's valuation window. It lands a block later, so it
/// is judged by the slot it lands in, which may be the next one: on a chain
/// whose blocks fall on slot boundaries (the local devnet moves its clock an
/// hour a block) every block lands at a slot's start. A block interval that
/// couldn't be measured lands nowhere.
fn landing_slot(now: u64, interval: u64, window: u64, block_interval: u64) -> Option<u64> {
    let landing = now.checked_add(block_interval)?;
    (landing % interval.max(1) <= window).then(|| slot_of(landing, interval))
}

/// Whether to value the vault now: the valuation would land inside its
/// slot's window, and the vault hasn't been valued for that slot yet.
fn due_for_valuation(now: u64, interval: u64, window: u64, block_interval: u64, last: u64) -> bool {
    landing_slot(now, interval, window, block_interval).is_some_and(|slot| last < slot)
}

/// Whether anything priced is waiting to be settled. Settling when nothing
/// is would be paying for nothing.
async fn unsettled(client: &Client, vault: Address) -> bool {
    client
        .read(vault, IStationVault::unsettledCall {})
        .await
        .unwrap_or(false)
}

/// Whether the vault is known to have nothing priced waiting to be settled.
/// A freeze waits while something is, or while the read fails: the settle may
/// pay whoever the freeze is for.
async fn settled(client: &Client, vault: Address) -> bool {
    client
        .read(vault, IStationVault::unsettledCall {})
        .await
        .is_ok_and(|unsettled| !unsettled)
}

/// Settle up to `max` of what is priced, if anything is. Whether a settle
/// was sent.
async fn settle(client: &Client, keeper: Address, vault: Address, max: U256, what: &str) -> bool {
    if !unsettled(client, vault).await {
        return false;
    }
    send(client, keeper, vault, IVault::settleCall { max }, what).await;
    true
}

/// How many requests a round's own settle takes on at once.
const SETTLE_BATCH: u64 = 100;

/// The longest a vault goes without a priced valuation when nothing waits
/// for one. Nothing in the vault needs valuations at any pace: a request is
/// priced at the first valuation after it, the fees are measured on the
/// payday's own valuation and the holding fee by the second, and every
/// fallback counts from the request or claim, not from the last valuation.
/// What a valuation's age does change is what the vault's page, the
/// executor's estimate of what to bring home, and the idle floor's measure
/// of the stations read: each is the last priced valuation's. A day bounds
/// that, at one valuation a day for a vault nobody moves in.
const HEARTBEAT: u64 = DAY;

/// Monday 00:00 UTC of the week after the one `t` falls in, as the vault
/// counts it for its payday.
fn next_monday(t: u64) -> u64 {
    // The first of January 1970 was a Thursday, three days after a Monday.
    let since_monday = t / DAY + 3;
    (since_monday - since_monday % 7 + 4) * DAY
}

/// The vault's halt as the round read it, for whether its close waits on a
/// valuation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Halt {
    /// Not halted.
    Running,
    /// Halted.
    Halted,
    /// Whether it has halted couldn't be read.
    Unread,
}

/// Why the vault wants a valuation in `slot`, if it does. Each valuation
/// costs the platform a transaction, so one is sent only when something
/// waits for it, as the vault itself says:
///
/// - a request (a deposit, a withdrawal, a claim the cash covers, a tank
///   fill) that a valuation in this slot would price: the vault names the
///   oldest one no valuation has priced;
/// - the weekly payday, due at the first priced valuation on or after the
///   Monday after the last one, with or without anything queued;
/// - a close staged at the home station has ended since the last priced
///   valuation, and a claim or a unit holder waits to be served: the vault
///   serves neither until one is priced. Whether one waits is read in the
///   same round, not taken from the last look at the claims;
/// - a holder's units the vault won't serve until a newer priced valuation
///   counts their station as it is now (see [`units_wait_for_a_valuation`]);
/// - a halted vault's close, when the vault says a valuation in this slot
///   would find its home station empty and none since the halt has found
///   every station so (on a vault that trades on other chains too, their
///   posts decide);
/// - [`HEARTBEAT`]: a day since the last priced valuation, or none ever.
///
/// Whatever can't be read is taken as wanting one: a valuation too many
/// costs a transaction, one too few keeps somebody waiting.
async fn valuation_wanted(
    client: &Client,
    keeper: Address,
    vault: Address,
    slot: u64,
    halt: Halt,
    memory: &Memory,
) -> Option<&'static str> {
    let (priced, index) = match client.read(vault, IVault::lastPricedCall {}).await {
        Ok(last) if last.priced => (last.time, last.index),
        Ok(_) => return Some("never priced"),
        Err(_) => return Some("the last priced valuation could not be read"),
    };
    if slot >= priced.saturating_add(HEARTBEAT) {
        return Some("a day since the last priced valuation");
    }
    match client.read(vault, IVault::lastPaymentCall {}).await {
        Ok(paid) if slot < next_monday(paid) => {}
        Ok(_) => return Some("payday"),
        Err(_) => return Some("the last payday could not be read"),
    }
    match client.read(vault, IVault::nextUnpricedCall {}).await {
        Ok(earliest) if earliest == 0 || earliest > slot => {}
        Ok(_) => return Some("a request waits for its price"),
        Err(_) => return Some("the requests could not be read"),
    }
    match client.read(vault, IVault::needsValuationFirstCall {}).await {
        Ok(false) => {}
        Ok(true) => {
            if let Some(why) = claims_or_units_wait(client, vault, memory).await {
                return Some(why);
            }
        }
        Err(_) => return Some("whether a staged close ended could not be read"),
    }
    if let Some(why) = units_wait_for_a_valuation(client, keeper, vault, index, memory).await {
        return Some(why);
    }
    match halt {
        Halt::Running => None,
        Halt::Unread => Some("whether it has halted could not be read"),
        Halt::Halted => match client
            .read(vault, IVault::closeWantsValuationCall { time: slot })
            .await
        {
            Ok(false) => None,
            Ok(true) => Some("its close"),
            Err(_) => Some("whether its close waits for one could not be read"),
        },
    }
}

/// Why a staged close's end keeps somebody waiting for a valuation, if it
/// does: a claim booked or waiting to be tried, or a holder's units, read
/// now. What can't be read is taken as waiting.
async fn claims_or_units_wait(
    client: &Client,
    vault: Address,
    memory: &Memory,
) -> Option<&'static str> {
    match client.read(vault, IVault::unitsCall {}).await {
        Ok(units) if units.total.is_zero() => {}
        Ok(_) => return Some("a staged close ended, and units wait"),
        Err(_) => return Some("a staged close ended, and the units could not be read"),
    }
    let count: u64 = match client.read(vault, IVault::claimCountCall {}).await {
        Ok(count) => count.try_into().unwrap_or(u64::MAX),
        Err(_) => return Some("a staged close ended, and the claims could not be read"),
    };
    for id in memory.claims_from.max(1)..=count {
        match client
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(id),
                },
            )
            .await
        {
            Ok(claim) if matches!(claim.state, ClaimState::Pending | ClaimState::Waiting) => {
                return Some("a staged close ended, and a claim waits");
            }
            Ok(_) => {}
            Err(_) => return Some("a staged close ended, and a claim could not be read"),
        }
    }
    None
}

/// Why a holder's units wait for a valuation, if they do. A serve that
/// brings nothing home clears the units only as dust, judged on what the
/// home station holds now or what the last priced valuation (`index`)
/// counted at the stations, whichever is more: once the station has emptied
/// since that valuation, the vault refuses the serve (`NotYet`) until a newer
/// one counts it as it is. So one is wanted when a known holder's serve, as
/// the keeper sends it, is refused `NotYet`, nothing priced waits to be
/// settled (the serve waits for that, and the round's settle sees to it),
/// and the home station now holds less than half what that valuation counted
/// at the stations: one that has emptied, not one whose mark has only moved
/// with the market, which a valuation wouldn't help. Past the refusal, what
/// can't be read is taken as wanting one.
async fn units_wait_for_a_valuation(
    client: &Client,
    keeper: Address,
    vault: Address,
    index: U256,
    memory: &Memory,
) -> Option<&'static str> {
    let mut refused = false;
    for &holder in &memory.holders {
        if serve_refused_not_yet(client, keeper, vault, holder).await {
            refused = true;
            break;
        }
    }
    if !refused || !settled(client, vault).await {
        return None;
    }
    let Ok(last) = client.read(vault, IVault::valuationAtCall { index }).await else {
        return Some("units wait, and the last priced valuation could not be read");
    };
    let counted = last
        .gross
        .saturating_sub(U256::from(1u64))
        .saturating_sub(last.cash);
    let mark = match client.read(vault, IVault::homeStationCall {}).await {
        Ok(station) => client.read(station, IStation::valueCall {}).await,
        Err(error) => Err(error),
    };
    match mark {
        Ok(value) if value.mark < counted / U256::from(2u64) => {
            Some("units wait for a valuation to count their emptied station")
        }
        Ok(_) => None,
        Err(_) => Some("units wait, and their station could not be read"),
    }
}

/// Whether the vault refuses `holder`'s serve, sent by the keeper now, with
/// `NotYet`.
async fn serve_refused_not_yet(
    client: &Client,
    keeper: Address,
    vault: Address,
    holder: Address,
) -> bool {
    use alloy::network::TransactionBuilder;
    use alloy::providers::Provider;
    use alloy::sol_types::{SolCall, SolError};
    let request = alloy::rpc::types::TransactionRequest::default()
        .with_from(keeper)
        .with_to(vault)
        .with_input(IVault::serveProRataCall { holder }.abi_encode());
    match client.provider().call(request).await {
        Ok(_) => false,
        Err(error) => {
            let error = anyhow::Error::new(error);
            execution_reverted(&error)
                && reverted_with(&error, IVault::NotYet::SELECTOR, "NotYet()")
        }
    }
}

/// Whether a call sent now would go through landing at `time`: `endpoint`
/// runs it as the keeper would send it, with the block's time set to `time`.
/// The vault's answer if it reverts; an error if the endpoint couldn't say.
/// Only an endpoint found to run a call at the time it is given is asked
/// (see [`endpoint_for_block_time`]): one that ran it at the head's time
/// instead would judge it by the wrong moment.
async fn would_go_through_at<C: alloy::sol_types::SolCall>(
    endpoint: &alloy::providers::DynProvider,
    keeper: Address,
    to: Address,
    call: C,
    time: u64,
) -> Result<bool> {
    use alloy::network::TransactionBuilder;
    use alloy::providers::Provider;
    let request = alloy::rpc::types::TransactionRequest::default()
        .with_from(keeper)
        .with_to(to)
        .with_input(call.abi_encode());
    match endpoint
        .call(request)
        .with_block_overrides(at_time(time))
        .await
    {
        Ok(_) => Ok(true),
        Err(error) => {
            let error = anyhow::Error::new(error);
            if execution_reverted(&error) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

/// A block whose time is `time`, for a call run at a set moment.
fn at_time(time: u64) -> alloy::rpc::types::BlockOverrides {
    alloy::rpc::types::BlockOverrides {
        time: Some(time),
        ..Default::default()
    }
}

/// Multicall3, at the one address it has on most chains (Base Sepolia,
/// HyperEVM's testnet and Robinhood Chain's among them): its read of the
/// block's time says what time an endpoint runs a call at.
const MULTICALL3: Address = Address::new([
    0xca, 0x11, 0xbd, 0xe0, 0x59, 0x77, 0xb3, 0x63, 0x11, 0x67, 0x02, 0x88, 0x62, 0xbe, 0x2a, 0x17,
    0x39, 0x76, 0xca, 0x11,
]);

alloy::sol! {
    /// Multicall3's read of the block's time.
    interface IMulticall3 {
        function getCurrentBlockTimestamp() external view returns (uint256 timestamp);
    }
}

/// Creation code that returns the block's time, for a chain without
/// Multicall3: `mstore(0, timestamp())`, `return(0, 32)`.
const BLOCK_TIME_CODE: [u8; 9] = [0x42, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];

/// How far past the head the probe of an endpoint sets the block's time:
/// far enough that no head it might be running at instead is mistaken for
/// it.
const BLOCK_TIME_PROBE_AHEAD: u64 = DAY;

/// Whether each endpoint runs a call at the block time it is given, by the
/// client's endpoint list and the endpoint's place in it, once found out.
/// Some ignore the time and run the call at the head's, with no error, so a
/// valuation simulated there near a slot's end would seem to revert either
/// way.
static RUNS_AT_BLOCK_TIME: std::sync::LazyLock<std::sync::Mutex<HashMap<(String, usize), bool>>> =
    std::sync::LazyLock::new(Default::default);

/// Whether `endpoint` runs a call at the block time it is given: it is asked
/// the time of a block set a day past `now`, through Multicall3, or through
/// creation code where nothing lives at Multicall3's address. `None` when it
/// couldn't be found out: it refused the time, or answered with nothing to
/// read.
async fn runs_at_block_time(endpoint: &alloy::providers::DynProvider, now: u64) -> Option<bool> {
    use alloy::network::TransactionBuilder;
    use alloy::providers::Provider;
    use alloy::sol_types::SolCall;
    let time = now.saturating_add(BLOCK_TIME_PROBE_AHEAD);
    let read = |returned: Bytes| (returned.len() == 32).then(|| U256::from_be_slice(&returned));
    let through_multicall = alloy::rpc::types::TransactionRequest::default()
        .with_to(MULTICALL3)
        .with_input(IMulticall3::getCurrentBlockTimestampCall {}.abi_encode());
    let mut answered = endpoint
        .call(through_multicall)
        .with_block_overrides(at_time(time))
        .await
        .ok()
        .and_then(read);
    if answered.is_none() {
        let created = alloy::rpc::types::TransactionRequest::default()
            .with_deploy_code(BLOCK_TIME_CODE.to_vec());
        answered = endpoint
            .call(created)
            .with_block_overrides(at_time(time))
            .await
            .ok()
            .and_then(read);
    }
    answered.map(|answered| answered == U256::from(time))
}

/// The endpoint a valuation is simulated at its landing through: the first
/// of the client's found to run a call at the block time it is given, and
/// its place in the list. Each is asked once (see [`runs_at_block_time`]),
/// and asked again only while that couldn't be found out, or after a
/// simulation through it failed. None when no endpoint is known to: the
/// simulation's verdict would then be the head's, not the landing's.
async fn endpoint_for_block_time(
    client: &Client,
    now: u64,
) -> Option<(usize, alloy::providers::DynProvider)> {
    for (at, endpoint) in client.each_endpoint().into_iter().enumerate() {
        let key = (client.rpc_url.clone(), at);
        let known = RUNS_AT_BLOCK_TIME
            .lock()
            .ok()
            .and_then(|known| known.get(&key).copied());
        let runs = match known {
            Some(runs) => runs,
            None => {
                let found = runs_at_block_time(&endpoint, now).await;
                if let Some(runs) = found {
                    if !runs {
                        tracing::info!(
                            chain_id = client.chain_id,
                            endpoint = at + 1,
                            "this endpoint runs a call at the head's time, whatever time it is \
                             given; valuations are not simulated at their landing through it"
                        );
                    }
                    if let Ok(mut known) = RUNS_AT_BLOCK_TIME.lock() {
                        known.insert(key, runs);
                    }
                }
                found.unwrap_or(false)
            }
        };
        if runs {
            return Some((at, endpoint));
        }
    }
    None
}

/// Forget what was found of an endpoint, after a call through it failed: it
/// is asked again before it is next trusted.
fn forget_block_time(client: &Client, at: usize) {
    if let Ok(mut known) = RUNS_AT_BLOCK_TIME.lock() {
        known.remove(&(client.rpc_url.clone(), at));
    }
}

/// The moments a valuation sent with the head at `now` is simulated at: when
/// it lands, a block later, and a block after that, when the chain's blocks
/// are close enough for both to fall inside one window. A send that misses
/// its block, or a head read from a node a block behind, lands a block late;
/// one judged only on the block it should land in then lands outside the
/// window, and pays for a revert.
fn landing_times(now: u64, window: u64, block_interval: u64) -> (u64, u64) {
    let landing = now.saturating_add(block_interval);
    let late = if block_interval <= window {
        landing.saturating_add(block_interval)
    } else {
        landing
    };
    (landing, late)
}

/// Send the slot's valuation, simulated first landing as it would and a
/// block late: one that would revert either way is the vault refusing it,
/// and is noted as refused without being sent; one that would only revert
/// late is left for the next slot, and nothing is noted. Neither is paid
/// for. It is simulated only through an endpoint found to run a call at the
/// time it is given (see [`endpoint_for_block_time`]); with none, or when
/// the simulation fails there, it is left to its gas estimate, as any other
/// send is. What became of it, if it was judged.
async fn simulated_valuation(
    client: &Client,
    keeper: Address,
    vault: Address,
    timers: &chain::bindings::vault::Timers,
    now: u64,
    block_interval: u64,
) -> Option<Outcome> {
    let (on_time, late) = landing_times(now, timers.slotWindow, block_interval);
    let call = || IVault::recordValuationCall {};
    let Some((at, endpoint)) = endpoint_for_block_time(client, now).await else {
        tracing::debug!(%vault, "no endpoint is known to run a call at the time it is given; the valuation's estimate judges it");
        return Some(send_outcome(client, keeper, vault, call(), "value").await);
    };
    let goes = match would_go_through_at(&endpoint, keeper, vault, call(), late).await {
        Ok(true) => true,
        Ok(false) if late == on_time => false,
        Ok(false) => match would_go_through_at(&endpoint, keeper, vault, call(), on_time).await {
            Ok(false) => false,
            Ok(true) => {
                tracing::debug!(%vault, "this slot's valuation could land after its window; not sent");
                return None;
            }
            Err(_) => {
                forget_block_time(client, at);
                return None;
            }
        },
        Err(error) => {
            forget_block_time(client, at);
            tracing::debug!(%vault, error = format!("{error:#}"), "the valuation could not be simulated at its landing; its estimate judges it");
            true
        }
    };
    if !goes {
        tracing::debug!(%vault, "the vault would refuse this slot's valuation; not sent");
        return Some(Outcome::Refused);
    }
    Some(send_outcome(client, keeper, vault, call(), "value").await)
}

/// Value the vault at the slot, if `value` and something waits for a
/// valuation (see [`valuation_wanted`]), and settle what has been priced.
#[allow(clippy::too_many_arguments)]
async fn price(
    client: &Client,
    keeper: Address,
    vault: Address,
    timers: &chain::bindings::vault::Timers,
    block_interval: u64,
    value: bool,
    halt: Halt,
    memory: &mut Memory,
) {
    // The round's other steps take time: the window is judged on the
    // chain's clock as it is now, not as it was when the round began.
    let Ok(now) = client.block_timestamp().await else {
        return;
    };
    let count = client
        .read(vault, IVault::valuationCountCall {})
        .await
        .unwrap_or_default();
    let latest = if count.is_zero() {
        None
    } else {
        client
            .read(
                vault,
                IVault::valuationAtCall {
                    index: count - U256::from(1u64),
                },
            )
            .await
            .ok()
    };
    // A valuation waiting for other chains' posts is closed once their
    // deadline has passed: priced, or void so its requests roll on.
    if let Some(open) = &latest
        && matches!(open.status, ValuationStatus::Open)
        && now > open.time + timers.postDeadline
    {
        send(
            client,
            keeper,
            vault,
            IVault::closeValuationCall { time: open.time },
            "close the valuation",
        )
        .await;
    }
    let last = latest.as_ref().map(|valuation| valuation.time).unwrap_or(0);
    if !value {
        memory
            .valuations_refused
            .not_wanted(Refused::Valuations, client.chain_id, vault);
    }
    let landing = landing_slot(now, timers.slotInterval, timers.slotWindow, block_interval);
    let wanted = match landing {
        Some(slot) if value && last < slot => {
            valuation_wanted(client, keeper, vault, slot, halt, memory).await
        }
        _ => None,
    };
    if value && landing.is_some_and(|slot| last < slot) && wanted.is_none() {
        memory
            .valuations_refused
            .not_wanted(Refused::Valuations, client.chain_id, vault);
    }
    if let Some(why) = wanted {
        tracing::debug!(%vault, why, "a valuation is wanted");
        // A valuation can't be recorded while the last one is still owed.
        let settled = settle(
            client,
            keeper,
            vault,
            U256::from(SETTLE_BATCH),
            "settle before the valuation",
        )
        .await;
        // The settle took a block of its own: whether the valuation still
        // lands inside a window it is due in is judged on the clock after it.
        let now = if settled {
            match client.block_timestamp().await {
                Ok(now) => now,
                Err(_) => return,
            }
        } else {
            now
        };
        // A valuation recorded while the station reads as outside the band,
        // or while a close is staged there, is void and prices nothing, so
        // none is paid for then.
        let due = due_for_valuation(
            now,
            timers.slotInterval,
            timers.slotWindow,
            block_interval,
            last,
        );
        if due && !close_staged(client, vault).await && !station_outside_band(client, vault).await {
            let outcome =
                simulated_valuation(client, keeper, vault, timers, now, block_interval).await;
            if let Some(outcome) = outcome {
                memory.valuations_refused.note(
                    outcome,
                    Refused::Valuations,
                    client.chain_id,
                    vault,
                    slot_of(now, timers.slotInterval),
                );
            }
        }
    }
    settle(client, keeper, vault, U256::from(SETTLE_BATCH), "settle").await;
}

/// When a waiting claim was last tried, as what, and when it may be tried
/// next.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tried {
    /// The claim's state when it was last looked at after a try, as the
    /// vault numbers it.
    state: u8,
    /// How many slots it was told to wait.
    wait: u64,
    /// The first slot it may be tried in again.
    next: u64,
    /// The first of the keeper's rounds it may be tried in again: a chain
    /// whose clock runs fast (the devnet moves an hour a block) passes many
    /// slots in a round, and the wait is whichever of the two is longer.
    next_round: u64,
}

/// The most slots a claim that keeps waiting is left between tries.
const MOST_SLOTS_BETWEEN_TRIES: u64 = 64;

/// Whether a claim in this state may be tried in this slot and round: never
/// tried, its state changed since, or its wait is over in slots and rounds.
fn claim_due(tried: Option<&Tried>, state: ClaimState, slot: u64, round: u64) -> bool {
    tried.is_none_or(|tried| {
        tried.state != state as u8 || (slot >= tried.next && round >= tried.next_round)
    })
}

/// When a claim may be tried again, given what it was after this slot's try.
/// One left waiting can't be paid from what the stations hold now, so it is
/// left one slot, then two, four and so on up to [`MOST_SLOTS_BETWEEN_TRIES`];
/// a change of state starts the count again.
fn next_try(
    before: Option<&Tried>,
    state: ClaimState,
    slot: u64,
    interval: u64,
    round: u64,
) -> Tried {
    let wait = match before {
        Some(before) if matches!(state, ClaimState::Waiting) && before.state == state as u8 => {
            (before.wait * 2).clamp(1, MOST_SLOTS_BETWEEN_TRIES)
        }
        _ => 1,
    };
    Tried {
        state: state as u8,
        wait,
        next: slot + wait * interval.max(1),
        next_round: round + wait,
    }
}

/// When a claim whose `settleClaim` never went out may be tried again: on
/// at the pace it was already waiting, without lengthening it.
fn not_tried(
    before: Option<&Tried>,
    state: ClaimState,
    slot: u64,
    interval: u64,
    round: u64,
) -> Tried {
    let wait = before.map(|before| before.wait).unwrap_or(1).max(1);
    Tried {
        state: state as u8,
        wait,
        next: slot + wait * interval.max(1),
        next_round: round + wait,
    }
}

/// Try the claims that are waiting, at most once a slot and less often the
/// longer each keeps waiting, and freeze a vault whose claim nobody has paid
/// for its whole freeze period. Such a claim is tried in that round first,
/// whatever its wait, and the vault frozen if it is still unpaid after, unless
/// a priced valuation is settling (see [`settled`]). A claim's close sells
/// nothing while any market is outside the band, so none is tried then; nor
/// while a close is staged at the home station (one claim's or one holder's,
/// see [`delayed`]), when the vault refuses every other. A claim that is
/// closing is never tried: it is paid when its close is delivered. A claim
/// that can't be read this round is left for the next; it holds up no other.
async fn claims(
    client: &Client,
    keeper: Address,
    vault: Address,
    timers: &chain::bindings::vault::Timers,
    halted: u64,
    memory: &mut Memory,
) -> Result<()> {
    // The round's earlier steps take time: the slot a try counts for, and
    // whether a claim has waited long enough to freeze, are judged on the
    // chain's clock as it is now.
    let now = client.block_timestamp().await?;
    let count: u64 = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .try_into()
        .unwrap_or(0);
    let slot = slot_of(now, timers.slotInterval);
    memory.claim_rounds += 1;
    let round = memory.claim_rounds;
    // Read once, and only if a claim is due. A read that fails says nothing
    // about the band, so the claims are tried anyway.
    let mut outside_band: Option<bool> = None;
    let mut staged: Option<bool> = None;
    let mut settled_prefix = true;
    // Whether any claim is still to be tried, or might be.
    let mut wanted = false;
    let mut unread = false;
    for id in turn_order(memory.claims_from.max(1), count, memory.last_staged) {
        let claim_id = U256::from(id);
        let claim = match client
            .read(vault, IVault::claimAtCall { claimId: claim_id })
            .await
        {
            Ok(claim) => claim,
            Err(error) => {
                carry_on(client.chain_id, vault, "read a claim", Err(error));
                settled_prefix = false;
                unread = true;
                continue;
            }
        };
        if matches!(claim.state, ClaimState::Closing) {
            memory.last_staged = Some(id);
        }
        if matches!(claim.state, ClaimState::Paid | ClaimState::Fallback)
            && memory.last_staged == Some(id)
        {
            memory.last_staged = None;
        }
        // Only past claims paid in id order: the turn may put an unpaid
        // claim after paid ones that come later.
        if settled_prefix {
            if matches!(claim.state, ClaimState::Paid | ClaimState::Fallback)
                && id == memory.claims_from.max(1)
            {
                memory.claims_from = id + 1;
                continue;
            }
            settled_prefix = false;
        }
        match claim.state {
            ClaimState::Pending
            | ClaimState::Waiting
            | ClaimState::Queued
            | ClaimState::Closing => {
                // Counted from its booking, as the vault counts it. A vault
                // already halted is stopping anyway.
                let overdue = halted == 0 && now >= claim.bookedAt + timers.freezeAfter;
                let tried = memory.claims_tried.get(&id).copied();
                // A claim about to freeze the vault is tried first, whatever
                // its wait: freezing halts everyone, and the cash or the
                // market may pay it now.
                let mut state = claim.state;
                // Whether a close staged at the station kept this claim from
                // its try this round: it is tried once that close has ended
                // (within its windows), never frozen for meanwhile.
                let mut held_off = matches!(claim.state, ClaimState::Closing);
                if claim_tryable(claim.state)
                    && (overdue || claim_due(tried.as_ref(), claim.state, slot, round))
                {
                    let held = match staged {
                        Some(held) => held,
                        None => {
                            let held = close_staged(client, vault).await;
                            staged = Some(held);
                            held
                        }
                    };
                    held_off |= held;
                    let wide = held
                        || match outside_band {
                            Some(wide) => wide,
                            None => {
                                let wide = station_outside_band(client, vault).await;
                                outside_band = Some(wide);
                                wide
                            }
                        };
                    if !wide {
                        // A claim's try values the station and stages its
                        // close inside try/catch, and counts a call that ran
                        // out of gas as blocked: with only the node's estimate
                        // (which finds that cheaper path) no claim the cash
                        // can't cover would ever be closed for.
                        let outcome = send_roomy_outcome(
                            client,
                            keeper,
                            vault,
                            IVault::settleClaimCall { claimId: claim_id },
                            "serve a claim",
                        )
                        .await;
                        memory.claims_refused.note(
                            outcome,
                            Refused::Claims,
                            client.chain_id,
                            vault,
                            slot,
                        );
                        // What the try left it as decides when it is next
                        // tried: a claim it couldn't pay waits longer. One
                        // refused before it was sent was never tried, and
                        // waits no longer than before.
                        let after = client
                            .read(vault, IVault::claimAtCall { claimId: claim_id })
                            .await
                            .map(|claim| claim.state)
                            .unwrap_or(claim.state);
                        let before = tried.filter(|tried| tried.state == claim.state as u8);
                        let next = if outcome.mined() {
                            next_try(before.as_ref(), after, slot, timers.slotInterval, round)
                        } else {
                            not_tried(before.as_ref(), after, slot, timers.slotInterval, round)
                        };
                        memory.claims_tried.insert(id, next);
                        if matches!(after, ClaimState::Closing) {
                            memory.last_staged = Some(id);
                            // This try staged a close: it holds off this
                            // claim and every claim after it this round.
                            staged = Some(true);
                            held_off = true;
                        }
                        state = after;
                    }
                }
                // A claim the try left unpaid (pending, waiting or queued)
                // freezes the vault, as the contract allows a week after
                // booking whatever was tried. Not while a priced valuation is
                // settling, or while that can't be read: its settle may pay
                // the claim, and a claim's try is refused until it is done.
                let unpaid = matches!(
                    state,
                    ClaimState::Pending
                        | ClaimState::Waiting
                        | ClaimState::Queued
                        | ClaimState::Closing
                );
                wanted |= matches!(state, ClaimState::Pending | ClaimState::Waiting);
                if overdue && unpaid && !held_off && settled(client, vault).await {
                    send(
                        client,
                        keeper,
                        vault,
                        IVault::freezeCall { claimId: claim_id },
                        "freeze",
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
    // No claim is left to try (paid, closed or taken): whatever the vault
    // refused of them is over.
    if !wanted && !unread {
        memory
            .claims_refused
            .not_wanted(Refused::Claims, client.chain_id, vault);
    }
    let from = memory.claims_from;
    memory.claims_tried.retain(|id, _| *id >= from);
    Ok(())
}

/// The claims from `from` to `count`, in the order they are tried: by id,
/// but the one a close was last staged for goes last, so a claim whose
/// close keeps ending with something left takes its turn after the others.
fn turn_order(from: u64, count: u64, last_staged: Option<u64>) -> Vec<u64> {
    let mut ids: Vec<u64> = (from..=count).collect();
    if let Some(last) = last_staged
        && let Some(at) = ids.iter().position(|id| *id == last)
    {
        let id = ids.remove(at);
        ids.push(id);
    }
    ids
}

/// Whether a claim in this state is tried: one booked or waiting. A queued
/// claim is priced by a valuation, and a closing one is paid when the close
/// staged for it is delivered; trying either does nothing.
fn claim_tryable(state: ClaimState) -> bool {
    matches!(state, ClaimState::Pending | ClaimState::Waiting)
}

/// Whether a close is staged at the vault's home station: while one is, the
/// vault tries no claim and serves no units, so nothing is sent for them. A
/// read that fails is taken as none staged; the vault refuses what it must.
async fn close_staged(client: &Client, vault: Address) -> bool {
    let Ok(station) = client.read(vault, IVault::homeStationCall {}).await else {
        return false;
    };
    client
        .read(station, IStation::stageCall {})
        .await
        .is_ok_and(|stage| stage.stagedAt != 0)
}

/// Whether the vault's home station reads as outside the band. A read that
/// fails is taken as inside: it proves nothing about the market, and a claim
/// that could be paid, or a valuation that could price, mustn't wait on a
/// read.
async fn station_outside_band(client: &Client, vault: Address) -> bool {
    let Ok(station) = client.read(vault, IVault::homeStationCall {}).await else {
        return false;
    };
    client
        .read(station, IStation::outsideBandCall {})
        .await
        .unwrap_or(false)
}

/// The most blocks one log read asks for: a node refuses a range much wider.
const LOG_CHUNK: u64 = 10_000;

/// The most log reads a vault gets in one round, so catching up on a long
/// history doesn't hold the other vaults up; it carries on next round.
const LOG_CHUNKS_PER_ROUND: usize = 10;

/// The fewest blocks a log read is cut down to when a node refuses a range.
const LEAST_LOG_CHUNK: u64 = 100;

/// The logs a filter matches from `start` on, reading at most up to `end`
/// and as far as the node will answer for in one read: a range it refuses,
/// or doesn't answer in time, or answers with an HTTP error, is halved, down
/// to [`LEAST_LOG_CHUNK`] blocks, since nodes and the gateways in front of
/// them differ in how wide a range they take. The logs, and the last block
/// they cover.
async fn logs_from(
    client: &Client,
    filter: &alloy::rpc::types::Filter,
    start: u64,
    end: u64,
) -> Result<(Vec<alloy::rpc::types::Log>, u64)> {
    let mut end = end;
    loop {
        let range = filter.clone().from_block(start).to_block(end);
        match client.logs(&range).await {
            Ok(logs) => return Ok((logs, end)),
            Err(error) => {
                let span = end - start + 1;
                if span <= LEAST_LOG_CHUNK {
                    return Err(error);
                }
                end = start + (span / 2).max(LEAST_LOG_CHUNK) - 1;
            }
        }
    }
}

/// Serve the units the pro-rata fallback left, and freeze a vault whose
/// units have waited unserved for as long as a claim may. Holders are found
/// from the vault's own log, read on from where the last round stopped.
/// Whether every holder is served, with the log read up to the head.
///
/// A holder's serve that was mined and reverted paid its gas and served
/// nothing, so that holder's is sent again only after a wait (see
/// [`send_again`]), until one goes through; one refused before it went out
/// costs nothing and is sent again next round.
#[allow(clippy::too_many_arguments)]
async fn units(
    client: &Client,
    keeper: Address,
    vault: Address,
    from_block: u64,
    now: u64,
    freeze_after: u64,
    slot_interval: u64,
    halted: u64,
    memory: &mut Memory,
) -> Result<bool> {
    // The holders already known are served even if the log can't be read
    // this round.
    let read_to_head = match find_holders(client, vault, from_block, memory).await {
        Ok(caught_up) => caught_up,
        Err(error) => {
            carry_on(client.chain_id, vault, "read who took units", Err(error));
            false
        }
    };

    let mut served = Vec::new();
    let mut unread = false;
    // A close staged at the home station holds every serve off; the one it is
    // for is credited when it is delivered.
    let held = !memory.holders.is_empty() && close_staged(client, vault).await;
    for &holder in &memory.holders {
        // A holder whose units can't be read this round is left for the
        // next; it holds up no other.
        let units = match client.read(vault, IVault::unitsOfCall { holder }).await {
            Ok(units) => units,
            Err(error) => {
                carry_on(client.chain_id, vault, "read a holder's units", Err(error));
                unread = true;
                continue;
            }
        };
        if units.units.is_zero() {
            served.push(holder);
            continue;
        }
        let waiting = held
            || memory
                .serve_again
                .get(&holder)
                .is_some_and(|again| now < again.next || memory.rounds < again.next_round);
        if !waiting {
            // Stages a close inside try/catch, like a claim's try: roomy gas,
            // or the estimate finds the path where nothing comes home.
            match send_roomy_outcome(
                client,
                keeper,
                vault,
                IVault::serveProRataCall { holder },
                "serve a holder's units",
            )
            .await
            {
                Outcome::Done => {
                    memory.serve_again.remove(&holder);
                }
                Outcome::Reverted => {
                    let again = send_again(
                        memory.serve_again.get(&holder),
                        now,
                        slot_interval,
                        memory.rounds,
                    );
                    memory.serve_again.insert(holder, again);
                }
                Outcome::Refused | Outcome::Stale | Outcome::NotSent => {}
            }
        }
        // Units the serve has just paid out have nothing left to freeze for.
        let since = match client.read(vault, IVault::unitsOfCall { holder }).await {
            Ok(units) if units.units.is_zero() => {
                served.push(holder);
                continue;
            }
            Ok(_) => client.read(vault, IVault::unitsSinceCall { holder }).await,
            Err(error) => Err(error),
        };
        let since = match since {
            Ok(since) => since,
            Err(error) => {
                carry_on(client.chain_id, vault, "read a holder's units", Err(error));
                unread = true;
                continue;
            }
        };
        // Not while a priced valuation is settling, or while that can't be
        // read: the serve is refused until it is done, and may pay them then.
        // Nor while a staged close holds every serve off, as for claims: the
        // serve is tried once it has ended (within its windows).
        if units_freeze_due(halted, now, since, freeze_after, held) && settled(client, vault).await
        {
            send(
                client,
                keeper,
                vault,
                IVault::freezeUnitsCall { holder },
                "freeze for units left unserved",
            )
            .await;
        }
    }
    // Units are only ever added by a new fallback, which the log shows.
    for holder in served {
        memory.holders.remove(&holder);
        memory.serve_again.remove(&holder);
    }
    Ok(read_to_head && !unread && memory.holders.is_empty())
}

/// Whether a holder's units left unserved since `since` are due a freeze:
/// a week (`freeze_after`) after, in a vault not halted, and not while a
/// close staged at the home station holds their serve off.
fn units_freeze_due(halted: u64, now: u64, since: u64, freeze_after: u64, held: bool) -> bool {
    halted == 0 && !held && now >= since.saturating_add(freeze_after)
}

/// Read on through the vault's log for whoever has taken units, from where
/// the last read stopped. Whether it has been read up to the head.
async fn find_holders(
    client: &Client,
    vault: Address,
    from_block: u64,
    memory: &mut Memory,
) -> Result<bool> {
    let head = client.block_number().await?;
    let filter = alloy::rpc::types::Filter::new()
        .address(vault)
        .event_signature(<IVault::ProRataTaken as alloy::sol_types::SolEvent>::SIGNATURE_HASH);
    for _ in 0..LOG_CHUNKS_PER_ROUND {
        let start = memory.holders_read_to.max(from_block);
        if start > head {
            break;
        }
        let (logs, end) = logs_from(
            client,
            &filter,
            start,
            start.saturating_add(LOG_CHUNK - 1).min(head),
        )
        .await?;
        for log in logs {
            if let Ok(taken) = log.log_decode::<IVault::ProRataTaken>()
                && !taken.inner.data.units.is_zero()
            {
                memory.holders.insert(taken.inner.data.owner);
            }
        }
        memory.holders_read_to = end + 1;
    }
    Ok(memory.holders_read_to.max(from_block) > head)
}

/// A closed vault pays out what was still waiting when it closed: requests
/// asked for after its last valuation (a withdrawal at the closed price, a
/// deposit sent back) and claims not yet paid. Each pays its own owner, so
/// anyone may send them. Whether every request and claim is done.
async fn paid_out(
    client: &Client,
    keeper: Address,
    vault: Address,
    memory: &mut Memory,
) -> Result<bool> {
    let requests: u64 = client
        .read(vault, IVault::requestCountCall {})
        .await?
        .try_into()
        .unwrap_or(0);
    let mut settled_prefix = true;
    for id in memory.requests_from.max(1)..=requests {
        let request_id = U256::from(id);
        // A request that can't be read this round is left for the next.
        let request = match client
            .read(
                vault,
                IVault::requestAtCall {
                    requestId: request_id,
                },
            )
            .await
        {
            Ok(request) => request,
            Err(error) => {
                carry_on(client.chain_id, vault, "read a request", Err(error));
                settled_prefix = false;
                continue;
            }
        };
        if request.done || matches!(request.kind, RequestKind::Claim) {
            if settled_prefix {
                memory.requests_from = id + 1;
            }
            continue;
        }
        settled_prefix = false;
        send(
            client,
            keeper,
            vault,
            IVault::settleClosedCall {
                requestId: request_id,
            },
            "settle a request at the closed price",
        )
        .await;
    }
    let claims: u64 = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .try_into()
        .unwrap_or(0);
    let mut settled_prefix = true;
    for id in memory.claims_from.max(1)..=claims {
        let claim_id = U256::from(id);
        // A claim that can't be read this round is left for the next.
        let claim = match client
            .read(vault, IVault::claimAtCall { claimId: claim_id })
            .await
        {
            Ok(claim) => claim,
            Err(error) => {
                carry_on(client.chain_id, vault, "read a claim", Err(error));
                settled_prefix = false;
                continue;
            }
        };
        if settled_prefix {
            if matches!(claim.state, ClaimState::Paid | ClaimState::Fallback) {
                memory.claims_from = id + 1;
                continue;
            }
            settled_prefix = false;
        }
        if matches!(
            claim.state,
            ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
        ) {
            send(
                client,
                keeper,
                vault,
                IVault::claimClosedCall { claimId: claim_id },
                "pay a claim from the closed vault",
            )
            .await;
        }
    }
    Ok(memory.requests_from.max(1) > requests && memory.claims_from.max(1) > claims)
}

/// Whether the vault has halted and its grace is over, so its station is
/// to be stopped and emptied and the vault closed.
async fn grace_is_over(client: &Client, vault: Address, now: u64, halted: u64) -> Result<bool> {
    Ok(grace_end(client, vault, halted)
        .await?
        .is_some_and(|end| now >= end))
}

/// When the halt's grace ends, if the vault has halted.
async fn grace_end(client: &Client, vault: Address, halted: u64) -> Result<Option<u64>> {
    if halted == 0 {
        return Ok(None);
    }
    let grace = client.read(vault, IStationVault::haltGraceCall {}).await?;
    Ok(Some(halted.saturating_add(grace)))
}

/// Ask the executor for a halted vault's total when the halt is first seen
/// (anyone may close the vault inside the grace once a valuation finds its
/// stations empty, and its author may then take the tank), and again once
/// the grace is over, when one total covers all of it. The usage cap is
/// cumulative, so newer totals in between lose nothing by waiting for that
/// one and aren't each worth a transaction. Each answer is collected once it
/// has come back (see [`collect_answered`]). Once the vault can charge
/// nothing more (see [`nothing_to_charge`]) it isn't asked.
fn ask_halted(
    chain_id: u64,
    executor: &str,
    vault: Address,
    tank: &Tank,
    now: u64,
    grace_end: u64,
    memory: &mut Memory,
) {
    if nothing_to_charge(tank, Some(grace_end)).is_some() {
        return;
    }
    let asked = if now < grace_end {
        &mut memory.asked_at_halt
    } else {
        &mut memory.asked_after_grace
    };
    // An ask already in flight was sent before this point: one is sent
    // once it is back.
    if !*asked && ask(chain_id, executor, vault, Lane::Halted).is_some() {
        *asked = true;
    }
}

/// Before a send that may close a halted vault: its author may take the
/// tank the moment it closes, so whatever newer total the executor has
/// answered with is collected first. Not once the vault can charge nothing
/// more (see [`nothing_to_charge`]).
///
/// With `ask`, for `close`, which has no window, the executor is asked
/// (unless an ask is already in flight) and the answer waited for, up to
/// [`CLOSE_WAIT`]. Without, for the one-transaction close, which must land
/// inside a slot's window, only an ask already in flight (the one sent once
/// the grace was over, in the same round) is waited for, unless the close is
/// held for its bill: then the executor is asked as for `close`. Either way
/// the close is judged again after it on whether it still lands inside the
/// window.
///
/// Whether the close may go: once nothing is left to collect with the
/// latest answer in hand (see [`Billing::settled`]). A `keep` the node turned
/// away or the vault refused holds the close for a later round, as does a
/// tank that can't be read, but for no more than [`MOST_CLOSE_HOLD`] from
/// the round (`now`, chain time) the close was first held, both in chain
/// time and on the keeper's clock (see [`waited`]). The hold ends only when
/// the bill settles (see [`bill_settled`]), wherever in the round that
/// happens: a round that finds the close impossible, a read that fails, or
/// the Closer's path in place of `close`'s, never starts it again. Once the
/// hold is over the latest total is tried once more, whatever wait a
/// reverted `keep` left (one carrying it mined and reverted earlier in the
/// same round was that try); if that try reaches the vault and the vault
/// refuses it (its estimate reverts, or it is mined and reverts), the bill is
/// written off and the close goes. A try that doesn't reach the vault (the node
/// turned it away, the tank couldn't be read, or the vault answered `Stale`,
/// which a node behind the chain also says) is made again each round for as
/// long again as the hold, on both clocks, from when it was first made; after
/// that the bill is written off and the close goes all the same: depositors'
/// money isn't held for good for a bill that can't be collected. A write-off
/// is said at warn, and once the bill is written off for a halt no newer total
/// holds the close again (newer totals are still collected).
#[allow(clippy::too_many_arguments)]
async fn collect_before_close(
    client: &Client,
    keeper: Address,
    executor: &str,
    vault: Address,
    grace_end: u64,
    now: u64,
    slot_interval: u64,
    ask_first: bool,
    memory: &mut Memory,
) -> bool {
    let written_off = memory.written_off_for == Some(grace_end);
    // The hold is over: the latest total is tried once more, whatever wait a
    // reverted `keep` left, and the bill written off if the vault refuses it.
    let last_try = !written_off
        && memory
            .close_held_since
            .is_some_and(|since| waited(since, now));
    let collected = async {
        let tank = client.read(vault, IVault::tankCall {}).await?;
        if nothing_to_charge(&tank, Some(grace_end)).is_none() {
            // A hold waits on the latest total, so the Closer's path asks
            // for it too.
            if ask_first || memory.close_held_since.is_some() {
                ask(client.chain_id, executor, vault, Lane::Halted);
            }
            answer_in_flight(executor, client.chain_id, vault, Lane::Halted, CLOSE_WAIT).await;
        }
        let (billing, _) = collect_answered(
            client,
            keeper,
            executor,
            vault,
            &tank,
            Some(grace_end),
            now,
            slot_interval,
            last_try,
            memory,
            "collect the bill before the vault closes",
        )
        .await;
        Ok(billing)
    };
    let billing = match collected.await {
        Ok(billing) => Some(billing),
        Err(error) => {
            carry_on(
                client.chain_id,
                vault,
                "collect the bill before the vault closes",
                Err(error),
            );
            None
        }
    };
    if written_off {
        return true;
    }
    if billing.is_some_and(Billing::settled) {
        bill_settled(client.chain_id, vault, memory);
        return true;
    }
    if last_try {
        // A last try that reached the vault and was refused writes the bill
        // off at once; one that didn't reach it is made again, but only for
        // so long.
        let refused = billing == Some(Billing::Refused);
        let tried_out = memory
            .last_try_since
            .is_some_and(|since| waited(since, now));
        if refused || tried_out {
            tracing::warn!(
                chain_id = client.chain_id,
                %vault,
                refused,
                "the bill has held the vault's close as long as it may; the vault is closed with it uncollected"
            );
            refused_bill_settled(client.chain_id, vault, memory);
            memory.written_off_for = Some(grace_end);
            return true;
        }
        memory
            .last_try_since
            .get_or_insert((now, std::time::Instant::now()));
    }
    memory
        .close_held_since
        .get_or_insert((now, std::time::Instant::now()));
    tracing::debug!(%vault, "the bill did not go through; the close waits for it");
    false
}

/// The keeper counts the vault's bill settled although the vault refused
/// the `keep` carrying it: it no longer tries that total (a newer one is
/// still collected), so the vault is no longer said to refuse it.
fn refused_bill_settled(chain_id: u64, vault: Address, memory: &mut Memory) {
    memory.bill_sent_as_of = memory.bill_sent_as_of.max(memory.bill_refused_as_of);
    bill_settled(chain_id, vault, memory);
}

/// The vault's bill is settled: a `keep` went through, there is nothing to
/// collect, the vault can charge nothing more, or the keeper wrote the bill
/// off. The vault is no longer said to refuse it, and a close held for it is
/// held no longer: a later wait gets a hold of its own. The only thing that
/// ends a close's hold. Whether the vault had been said to refuse it.
fn bill_settled(chain_id: u64, vault: Address, memory: &mut Memory) -> bool {
    memory.close_held_since = None;
    memory.last_try_since = None;
    memory.bill_refused = 0;
    if let Ok(mut refused) = BILLS_REFUSED.lock() {
        refused.remove(&(chain_id, vault));
    }
    memory.bill_refused_said.over()
}

/// What a simulation of the stopped station's full unwind says.
#[derive(Clone, Copy, Debug)]
struct Unwind {
    /// What the unwind would bring home.
    sent_home: U256,
    /// Whether a market outside the band would stop it.
    blocked: bool,
    /// The station's mark, when it can be valued.
    mark: Option<U256>,
    /// The station's mark after the unwind, when that was simulated.
    mark_after: Option<U256>,
    /// The vault's dust threshold: no more than this brought home is not
    /// worth an unwind of its own, nor less than a cent.
    dust: U256,
}

/// Whether a simulation shows the stopped station's unwind worth sending:
/// it would bring home enough to pay for itself (see [`worth_an_unwind`]),
/// or it would empty the station, worth that much now and less than that
/// after. A levered account under water at the bid is closed into its own
/// debt, which brings nothing home and yet empties the station, and an empty
/// station is what lets the vault close. Anything else is not sent: a
/// remnant the venue values at the mark and never sells, a stranger's gift
/// of dust, a market outside the band. Only what the simulation shows
/// counts, never how the mark moves between readings: a live market's mark
/// moves on its own.
fn worth_sending(would: &Unwind) -> bool {
    if worth_an_unwind(would.sent_home, would.dust) {
        return true;
    }
    !would.blocked
        && would
            .mark
            .is_some_and(|mark| worth_an_unwind(mark, would.dust))
        && would
            .mark_after
            .is_some_and(|after| !worth_an_unwind(after, would.dust))
}

/// When a simulation that found the unwind not worth sending is made again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Backoff {
    /// How many slots it was told to wait.
    wait: u64,
    /// The first slot it may be made in again.
    next: u64,
    /// Whether nothing but the station itself changing could make the unwind
    /// worth sending: it isn't blocked, and the station can be valued.
    idle: bool,
}

/// The longest wait between simulations of an unwind not worth sending: a
/// station that starts to sell, or a market that comes back, is noticed
/// within it.
const MOST_SECONDS_BETWEEN_CHECKS: u64 = 5 * MINUTE;

/// When to simulate the unwind again, after one that found it not worth
/// sending: next slot, then two slots on, four and so on up to
/// [`MOST_SECONDS_BETWEEN_CHECKS`]. Simulations are reads, which cost
/// nothing; the wait only spares the node.
fn next_check(before: Option<&Backoff>, would: &Unwind, slot: u64, interval: u64) -> Backoff {
    let interval = interval.max(1);
    let most = (MOST_SECONDS_BETWEEN_CHECKS / interval).max(1);
    let wait = before
        .map(|before| (before.wait * 2).clamp(1, most))
        .unwrap_or(1);
    Backoff {
        wait,
        next: slot + wait * interval,
        idle: !would.blocked && would.mark.is_some(),
    }
}

/// The least an unwind must bring home, in dollar-units, to be sent: a cent.
const UNWIND_WORTH_AT_LEAST: u64 = 10_000;

/// Whether an unwind that brings this much home pays for itself: at least a
/// cent, and more than the vault's dust threshold. The dust threshold alone
/// is a few millionths of a dollar, so a stranger who sends the station that
/// much each slot would otherwise have the keeper pay a full unwind (every
/// driver's close, then a transfer home) every slot.
fn worth_an_unwind(amount: U256, dust: U256) -> bool {
    amount >= U256::from(UNWIND_WORTH_AT_LEAST) && amount > dust
}

/// The vault's dust threshold, `(drivers + 1)^2` dollar-units, from the
/// station's drivers: a remnant at every venue and the station's rounding at
/// once. A station whose drivers can't be read has none, so whatever its
/// unwind would bring home is worth it.
async fn dust_of(client: &Client, station: Address) -> U256 {
    match client.read(station, IStation::specCall {}).await {
        Ok(spec) => {
            let parts = U256::from(spec.drivers.len() as u64 + 1);
            parts * parts
        }
        Err(_) => U256::ZERO,
    }
}

/// Stop the station and bring everything home. Whether the vault closed
/// along the way.
#[allow(clippy::too_many_arguments)]
async fn stop_and_unwind(
    client: &Client,
    keeper: Address,
    executor: &str,
    closer: Address,
    vault: Address,
    station: Address,
    timers: &chain::bindings::vault::Timers,
    grace_end: u64,
    block_interval: u64,
    memory: &mut Memory,
) -> Result<bool> {
    // Judged on the chain's clock as it is now: the round's earlier steps
    // take time.
    let now = client.block_timestamp().await?;
    // When it would land inside a slot's valuation window, the whole close
    // goes in one transaction, so nobody can get between the unwind and the
    // valuation. Otherwise, or if the close can't happen yet, the steps are
    // taken one by one so the station is still emptied. The close sells
    // inside the stopped station's band, which widens with time, so it isn't
    // sent while the stopped unwind reads as blocked: its valuation would
    // then be void and the close would fail with it. An unwind simulation
    // that can't be read (the station not stopped yet, say) doesn't hold it
    // off. The close itself is simulated first, and sent only if it would go
    // through: one that can't happen yet costs nothing. One that went through
    // and left the vault open isn't sent again until the vault's state
    // changes: the steps are taken instead.
    let closer_closed_nothing = match memory.closer_closed_nothing {
        Some(seen) => match client.read(vault, IVault::stateCall {}).await {
            Ok(state) if state as u8 == seen => true,
            Ok(_) => {
                memory.closer_closed_nothing = None;
                false
            }
            Err(_) => true,
        },
        None => false,
    };
    if closer != Address::ZERO
        && !closer_closed_nothing
        && landing_slot(now, timers.slotInterval, timers.slotWindow, block_interval).is_some()
        && !client
            .read(
                station,
                IStation::unwindCall {
                    fraction: U256::from(WAD),
                },
            )
            .await
            .is_ok_and(|would| would.blocked)
        && client
            .read(closer, ICloser::closeHaltedCall { vault })
            .await
            .inspect_err(|error| {
                tracing::debug!(%vault, error = format!("{error:#}"), "not closable in one go yet");
            })
            .is_ok()
    {
        // A bill that didn't go through holds the close for a later round.
        let billed = collect_before_close(
            client,
            keeper,
            executor,
            vault,
            grace_end,
            now,
            timers.slotInterval,
            false,
            memory,
        )
        .await;
        // The bill may have taken a block of its own: whether the close
        // still lands inside the window is judged on the clock after it.
        let still_lands = billed
            && client.block_timestamp().await.is_ok_and(|now| {
                landing_slot(now, timers.slotInterval, timers.slotWindow, block_interval).is_some()
            });
        if still_lands {
            // One that doesn't go out is heard of as any other send is.
            match send_roomy_outcome(
                client,
                keeper,
                closer,
                ICloser::closeHaltedCall { vault },
                "close the halted vault in one go",
            )
            .await
            {
                // A call that went through closed the vault only if it now
                // reads closed: one to an address with nothing deployed goes
                // through and does nothing, and taking it as a close would
                // skip the valuation the vault needs to close at all.
                Outcome::Done => {
                    match client.read(vault, IVault::stateCall {}).await {
                        Ok(VaultState::Closed) => {
                            tracing::info!(%vault, "closed the halted vault");
                            return Ok(true);
                        }
                        Ok(state) => memory.closer_closed_nothing = Some(state as u8),
                        Err(_) => {}
                    }
                    tracing::warn!(%vault, %closer, "the Closer's call went through and the vault is not closed; it is closed step by step until its state changes");
                }
                Outcome::Reverted | Outcome::Refused | Outcome::Stale | Outcome::NotSent => {
                    tracing::debug!(%vault, "not closed in one go");
                }
            }
        }
    }
    if !client.read(station, IStation::stoppedCall {}).await? {
        send(
            client,
            keeper,
            station,
            IStation::stop_0Call {},
            "stop the station",
        )
        .await;
    }
    unwind_stopped(client, keeper, vault, station, timers, now, memory).await?;
    Ok(false)
}

/// Ask a stopped station to unwind in full, at most once a slot, when a
/// simulation shows it worth sending (see [`worth_sending`]). A simulation
/// that finds it not worth sending is made again on its own wait (see
/// [`next_check`]); one that finds it worth sending is followed by another
/// next slot, so a station that keeps selling is unwound every slot.
async fn unwind_stopped(
    client: &Client,
    keeper: Address,
    vault: Address,
    station: Address,
    timers: &chain::bindings::vault::Timers,
    now: u64,
    memory: &mut Memory,
) -> Result<()> {
    let slot = slot_of(now, timers.slotInterval);
    if memory.unwound_in == Some(slot)
        || memory
            .unwind_checks
            .is_some_and(|checks| slot < checks.next)
    {
        return Ok(());
    }
    let dust = dust_of(client, station).await;
    // A stopped station unwinds only once the last priced valuation has
    // settled, so what the sale brings home never pays a withdrawal priced
    // before it; and the simulation below would fail until it has.
    settle(client, keeper, vault, U256::MAX, "settle before the unwind").await;
    // A venue that acts after the transaction can't show in a simulation
    // what the unwind brings home: its unwind is repeated while anything is
    // left there (see [`delayed::unwind_stopped`]).
    // Drivers that can't be read are taken as none: the unwind below is
    // then simulated as for any station, and a delayed one reads as blocked
    // there and sends nothing.
    let delayed = delayed::drivers_of(client, station, &mut memory.delayed)
        .await
        .inspect_err(|error| tracing::debug!(%vault, error = format!("{error:#}"), "the station's delayed drivers were not read"))
        .unwrap_or_default();
    if !delayed.is_empty() {
        memory.unwound_in = Some(slot);
        return delayed::unwind_stopped(
            client,
            keeper,
            vault,
            station,
            &delayed,
            now,
            timers.slotInterval,
            |amount| worth_an_unwind(amount, dust),
            &mut memory.delayed,
        )
        .await;
    }
    match client.read(station, IStation::valueCall {}).await {
        Ok(value) => {
            let fraction = U256::from(WAD);
            let simulated = client
                .read(station, IStation::unwindCall { fraction })
                .await
                .context("simulating the unwind")?;
            let mut would = Unwind {
                sent_home: simulated.sentHome,
                blocked: simulated.blocked,
                mark: Some(value.mark),
                mark_after: None,
                dust,
            };
            // What the station would hold after it is asked for only when
            // nothing worth it would come home and there is something to
            // empty. A simulation of that which fails sends nothing: it is
            // made again next slot, and a vault whose simulations keep
            // failing is said (see [`MOST_PROBES_FAILING`]).
            if !worth_an_unwind(would.sent_home, dust)
                && !would.blocked
                && worth_an_unwind(value.mark, dust)
            {
                match mark_after_unwind(client, station).await {
                    Ok(after) => would.mark_after = Some(after),
                    Err(error) => {
                        memory.probes_failing += 1;
                        tracing::debug!(%vault, error = format!("{error:#}"), "what the unwind leaves was not simulated");
                        if memory.probes_failing >= MOST_PROBES_FAILING
                            && let Some(times) = memory.probe_said.due(std::time::Instant::now())
                        {
                            tracing::warn!(%vault, times, failing = memory.probes_failing, error = format!("{error:#}"), "what the stopped station's unwind leaves can't be simulated; nothing is sent until it can");
                        }
                        // Not idle: whether the unwind is worth sending is
                        // not known, so a closed vault is kept in the round
                        // for it rather than left quiet.
                        memory.unwind_checks = Some(Backoff {
                            wait: memory.unwind_checks.map_or(0, |checks| checks.wait),
                            next: slot.saturating_add(timers.slotInterval.max(1)),
                            idle: false,
                        });
                        return Ok(());
                    }
                }
            }
            probes_not_failing(vault, memory);
            if !checked(memory, &would, slot, timers.slotInterval) {
                return Ok(());
            }
            send_roomy_outcome(
                client,
                keeper,
                station,
                IStation::unwindCall { fraction },
                "unwind the station",
            )
            .await;
        }
        // A station that can't be valued is still emptied, venue by venue,
        // through whichever of its drivers can close.
        Err(error) => {
            probes_not_failing(vault, memory);
            tracing::warn!(%vault, error = format!("{error:#}"), "the stopped station can't be valued; unwinding what can be");
            let spec = client.read(station, IStation::specCall {}).await?;
            let mut closable = Vec::new();
            for driver in spec.drivers {
                let tried = client
                    .read(
                        station,
                        IStation::unwindStoppedCall {
                            drivers: vec![driver],
                        },
                    )
                    .await;
                if tried.is_ok() {
                    closable.push(driver);
                }
            }
            let simulated = client
                .read(
                    station,
                    IStation::unwindStoppedCall {
                        drivers: closable.clone(),
                    },
                )
                .await
                .context("simulating the unwind through the drivers that can close")?;
            // With no mark to go by, only an unwind that brings something
            // home worth it is sent.
            let would = Unwind {
                sent_home: simulated.sentHome,
                blocked: simulated.blocked,
                mark: None,
                mark_after: None,
                dust,
            };
            if !checked(memory, &would, slot, timers.slotInterval) {
                return Ok(());
            }
            send_roomy_outcome(
                client,
                keeper,
                station,
                IStation::unwindStoppedCall { drivers: closable },
                "unwind what can be of the station",
            )
            .await;
        }
    }
    Ok(())
}

/// Note what a simulation of the unwind found, and whether to send it. One
/// worth sending is sent this slot, and simulated again next slot whatever
/// became of the send; one that isn't waits for its next simulation.
fn checked(memory: &mut Memory, would: &Unwind, slot: u64, interval: u64) -> bool {
    if !worth_sending(would) {
        memory.unwind_checks = Some(next_check(
            memory.unwind_checks.as_ref(),
            would,
            slot,
            interval,
        ));
        return false;
    }
    memory.unwind_checks = None;
    memory.unwound_in = Some(slot);
    true
}

/// Simulations in a row of what a vault's unwind leaves that may fail before
/// the vault is said at warn and in the keeper's health: one that fails now
/// and then (a rate limit, a busy node) is tried again next slot.
const MOST_PROBES_FAILING: u64 = 30;

/// The vault's unwind no longer waits on a simulation that keeps failing:
/// one worked, or none was needed.
fn probes_not_failing(vault: Address, memory: &mut Memory) {
    memory.probes_failing = 0;
    if memory.probe_said.over() {
        tracing::info!(%vault, "what the unwind leaves can be simulated again");
    }
}

/// Where the probe's code is put, for a node that runs a call only with a
/// contract to call: an address nothing lives at, given the probe's code for
/// that one call.
const PROBE_AT: Address = Address::new([
    0x5e, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x01,
]);

/// What a chain's node is asked to simulate when it connects: the probe of
/// an unwind, of a "station" at an address nothing lives at. Calls to it go
/// through and return nothing, so a node that runs the probe answers it.
const NOTHING_AT: Address = Address::new([
    0x5e, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x02,
]);

/// Whether the chain's node runs the simulation of what an unwind leaves
/// (see [`mark_after_unwind`]), in either form. The keeper sends no unwind
/// it hasn't simulated, so a chain whose node can't is not kept.
async fn can_simulate(client: &Client) -> Result<()> {
    mark_after_unwind(client, NOTHING_AT).await.map(|_| ())
}

/// How long a chain whose node can't simulate waits before it is checked
/// again, at first and at most: the wait doubles in between.
const SIMULATION_CHECK_FIRST: Duration = Duration::from_secs(5);
const SIMULATION_CHECK_MOST: Duration = Duration::from_secs(5 * 60);

/// Check that the chain's node can simulate, until it can: while it can't,
/// the chain is not kept, and that is said at warn and in the keeper's
/// health.
async fn until_it_can_simulate(chain: Chain, client: &Client, key: &impl std::fmt::Display) {
    let mut wait = SIMULATION_CHECK_FIRST;
    let mut said = Said::default();
    while let Err(error) = can_simulate(client).await {
        chain_kept(chain, |kept| kept.cannot_simulate = true);
        if let Some(times) = said.due(std::time::Instant::now()) {
            tracing::warn!(chain = %key, times, error = format!("{error:#}"), "this chain's node can't simulate what an unwind leaves; the chain is not kept until it can");
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(SIMULATION_CHECK_MOST);
    }
    if said.over() {
        tracing::info!(chain = %key, "this chain's node simulates; it is kept");
    }
}

/// What the station's mark would be after its full unwind, from one
/// simulation of both: a contract that never lands asks the station to
/// unwind in full, which anyone may once it is stopped, then reads its value,
/// and returns what the unwind returned and the mark after. It is run as
/// creation code by `eth_call`; a node that won't run a call without a
/// contract to call is asked to run it as the code of [`PROBE_AT`] instead,
/// through the call's state override. A call the node answers with too
/// little to be the probe's answer has failed, as has one it gives no
/// answer to; it fails only if both ways do.
async fn mark_after_unwind(client: &Client, station: Address) -> Result<U256> {
    use alloy::network::TransactionBuilder;
    use alloy::providers::Provider;
    // The mark after, or why not.
    let read = |returned: Result<Bytes, alloy::transports::TransportError>| match returned {
        Ok(returned) if returned.len() >= 96 => Ok(U256::from_be_slice(&returned[64..96])),
        Ok(returned) => Err(format!("{} bytes back", returned.len())),
        Err(error) => Err(error.to_string()),
    };
    let probe = unwind_probe(station);
    let request = alloy::rpc::types::TransactionRequest::default().with_deploy_code(probe.clone());
    let created = match read(client.provider().call(request).await) {
        Ok(after) => return Ok(after),
        Err(failed) => failed,
    };
    let request = alloy::rpc::types::TransactionRequest::default().with_to(PROBE_AT);
    let overridden = client
        .provider()
        .call(request)
        .account_override(
            PROBE_AT,
            alloy::rpc::types::state::AccountOverride::default().with_code(probe),
        )
        .await;
    let overridden = match read(overridden) {
        Ok(after) => return Ok(after),
        Err(failed) => failed,
    };
    anyhow::bail!(
        "simulating the unwind and the value after it: as creation code: {created}; as an overridden contract's code: {overridden}"
    )
}

/// Creation code that calls `station.unwind(1e18)`, then `station.value()`,
/// and returns `(sentHome, blocked, markAfter)`; a call that fails makes it
/// revert with that call's own reason.
fn unwind_probe(station: Address) -> Vec<u8> {
    use alloy::sol_types::SolCall;
    const PUSH1: u8 = 0x60;
    // mstore(0, selector << 224)
    fn put_selector(code: &mut Vec<u8>, selector: [u8; 4]) {
        code.push(0x63);
        code.extend_from_slice(&selector);
        code.extend_from_slice(&[PUSH1, 0xe0, 0x1b, PUSH1, 0x00, 0x52]);
    }
    let mut code = Vec::new();
    let mut failed_at = Vec::new();
    put_selector(&mut code, IStation::unwindCall::SELECTOR);
    // mstore(4, 1e18)
    code.push(0x67);
    code.extend_from_slice(&WAD.to_be_bytes());
    code.extend_from_slice(&[PUSH1, 0x04, 0x52]);
    // call(gas, station, 0, 0, 36, 64, 64): the unwind's answer at 64..128.
    code.extend_from_slice(&[
        PUSH1, 0x40, PUSH1, 0x40, PUSH1, 0x24, PUSH1, 0x00, PUSH1, 0x00, 0x73,
    ]);
    code.extend_from_slice(station.as_slice());
    code.extend_from_slice(&[0x5a, 0xf1]);
    // if iszero(success) jump to the revert
    code.extend_from_slice(&[0x15, 0x61, 0x00, 0x00, 0x57]);
    failed_at.push(code.len() - 3);
    put_selector(&mut code, IStation::valueCall::SELECTOR);
    // staticcall(gas, station, 0, 4, 128, 64): the mark at 128..160.
    code.extend_from_slice(&[PUSH1, 0x40, PUSH1, 0x80, PUSH1, 0x04, PUSH1, 0x00, 0x73]);
    code.extend_from_slice(station.as_slice());
    code.extend_from_slice(&[0x5a, 0xfa]);
    code.extend_from_slice(&[0x15, 0x61, 0x00, 0x00, 0x57]);
    failed_at.push(code.len() - 3);
    // return(64, 96)
    code.extend_from_slice(&[PUSH1, 0x60, PUSH1, 0x40, 0xf3]);
    // the revert: returndatacopy(0, 0, size); revert(0, size)
    let revert = u16::try_from(code.len()).unwrap_or(u16::MAX).to_be_bytes();
    code.extend_from_slice(&[
        0x5b, 0x3d, PUSH1, 0x00, PUSH1, 0x00, 0x3e, 0x3d, PUSH1, 0x00, 0xfd,
    ]);
    for at in failed_at {
        code[at..at + 2].copy_from_slice(&revert);
    }
    code
}

/// When the author retired the vault, from its `StateChanged` log, read on
/// from where the last read stopped and remembered once found.
async fn retired_at(
    client: &Client,
    vault: Address,
    from_block: u64,
    memory: &mut Memory,
) -> Result<Option<u64>> {
    if let Some(at) = memory.retired_at {
        return Ok(Some(at));
    }
    let head = client.block_number().await?;
    let filter = alloy::rpc::types::Filter::new()
        .address(vault)
        .event_signature(<IVault::StateChanged as alloy::sol_types::SolEvent>::SIGNATURE_HASH);
    for _ in 0..LOG_CHUNKS_PER_ROUND {
        let start = memory.retired_read_to.max(from_block);
        if start > head {
            break;
        }
        let (logs, end) = logs_from(
            client,
            &filter,
            start,
            start.saturating_add(LOG_CHUNK - 1).min(head),
        )
        .await?;
        for log in logs.iter().rev() {
            let Ok(decoded) = log.log_decode::<IVault::StateChanged>() else {
                continue;
            };
            if matches!(decoded.inner.data.state, VaultState::WindingDown)
                && let Some(block) = log.block_number
            {
                let at = client.block_timestamp_at(block).await?;
                memory.retired_at = Some(at);
                return Ok(Some(at));
            }
        }
        memory.retired_read_to = end + 1;
    }
    Ok(None)
}

/// How long one ask of the executor may take.
const ASK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the round waits, before a `close`, for an answer to an ask
/// still in flight: as long as the ask itself may take.
const CLOSE_WAIT: Duration = ASK_TIMEOUT;
/// The most asks of one executor in flight at once, in each lane (see
/// [`Lane`]).
const ASKS_AT_ONCE: usize = 8;
/// How long the executor may go answering no ask before the keeper's health
/// says it isn't answering: an executor restarting, or one ask lost, is not
/// a trouble to show.
const UNANSWERED_FOR: Duration = Duration::from_secs(15 * 60);
/// The longest wait, in chain time, before an ask that got no answer is
/// made again.
const MOST_SECONDS_BETWEEN_ASKS: u64 = 60 * MINUTE;

/// Which of an executor's two lanes an ask waits its turn in. A halted or
/// closed vault's ask is wanted soon (a `close` waits for it, and a closed
/// vault's last bill has a window), so it never queues behind the open
/// vaults' daily asks, however many of those there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Lane {
    /// An open vault's daily ask.
    Daily,
    /// A halted or closed vault's.
    Halted,
}

static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(ASK_TIMEOUT)
        .build()
        .unwrap_or_default()
});

/// What the executor answered when asked for a vault's total.
#[derive(Clone)]
enum Asked {
    /// Its latest signed total.
    Total(Signed),
    /// It serves no total for the vault (a 404).
    NotServed,
}

/// One executor, by its URL, as every chain's rounds ask it.
struct Executor {
    asks: std::sync::Mutex<Asks>,
    /// Its asks in flight at once, whichever chain asks, so rounds that ask
    /// about many vaults at once can't open a connection for each; another
    /// executor's are its own. Each lane has its own.
    asking: tokio::sync::Semaphore,
    asking_halted: tokio::sync::Semaphore,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            asks: Default::default(),
            asking: tokio::sync::Semaphore::new(ASKS_AT_ONCE),
            asking_halted: tokio::sync::Semaphore::new(ASKS_AT_ONCE),
        }
    }
}

impl Executor {
    fn lane(&self, lane: Lane) -> &tokio::sync::Semaphore {
        match lane {
            Lane::Daily => &self.asking,
            Lane::Halted => &self.asking_halted,
        }
    }
}

/// What has been asked of one executor, by chain id and vault.
#[derive(Default)]
struct Asks {
    /// The latest answer for each vault, and when the ask it answered was
    /// sent: replaced only by the answer to a later ask.
    latest: HashMap<(u64, Address), (Asked, std::time::Instant)>,
    /// The vaults it is being asked about, in each lane, and when that ask
    /// was sent. A vault's ask in one lane doesn't hold back its ask in the
    /// other: a halted vault's ask is never held behind its own daily ask,
    /// queued behind the open vaults'.
    in_flight: HashMap<(u64, Address, Lane), std::time::Instant>,
    /// The vaults whose latest ask came back with no answer: until an ask
    /// about the vault is answered, or the keeper no longer wants a total for
    /// it. Each is asked again on its own wait (see [`ask_again`]).
    unanswered: BTreeSet<(u64, Address)>,
    /// When the keeper last said it no longer wants a vault's total: an ask
    /// sent before then that comes back with no answer doesn't count.
    not_wanted: HashMap<(u64, Address), std::time::Instant>,
    /// Since when no ask has been answered, while asks keep going
    /// unanswered.
    failing_since: Option<std::time::Instant>,
    /// The vaults it has answered it serves no total for, as said.
    not_served: BTreeSet<(u64, Address)>,
    /// Since when its latest answer for every vault the keeper still wants a
    /// total for has been a 404, while it is (see [`serving_none`]), and
    /// that warning.
    serving_none_since: Option<std::time::Instant>,
    serving_none_said: Said,
    /// Its asks that went unanswered, as said.
    said: Said,
}

impl Asks {
    /// Whether the keeper still wants a total for the vault, as of an ask
    /// sent at `sent`: it hasn't said since that it no longer does (see
    /// [`total_not_wanted`]).
    fn wanted(&self, key: &(u64, Address), sent: std::time::Instant) -> bool {
        self.not_wanted.get(key).is_none_or(|since| sent > *since)
    }

    /// Whether the executor's latest answer for the vault is a 404 while the
    /// keeper still wants its total: it is asked again (see [`ask_again`]).
    fn wanted_but_not_served(&self, key: &(u64, Address)) -> bool {
        self.latest.get(key).is_some_and(|(asked, sent)| {
            matches!(asked, Asked::NotServed) && self.wanted(key, *sent)
        })
    }

    /// Start or stop the time the executor has served no vault (see
    /// [`serving_none`]), counting only the vaults the keeper still wants a
    /// total for: it serves none while every such vault's latest answer is a
    /// 404, and not once any gets a total or none is wanted.
    fn note_serving(&mut self) {
        let mut wanted = self
            .latest
            .iter()
            .filter(|(key, (_, sent))| self.wanted(key, *sent))
            .peekable();
        let none = wanted.peek().is_some()
            && wanted.all(|(_, (asked, _))| matches!(asked, Asked::NotServed));
        if none {
            self.serving_none_since
                .get_or_insert_with(std::time::Instant::now);
        } else {
            self.serving_none_since = None;
        }
    }
}

/// Every executor the keeper asks, by its URL.
static EXECUTORS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, SharedExecutor>>> =
    std::sync::LazyLock::new(Default::default);

/// One executor, shared by the rounds and their asks.
type SharedExecutor = std::sync::Arc<Executor>;

fn executor_for(url: &str) -> SharedExecutor {
    let Ok(mut executors) = EXECUTORS.lock() else {
        return Default::default();
    };
    executors.entry(url.to_string()).or_default().clone()
}

/// Ask the executor for a vault's total, off the round, in a task of its
/// own: an executor that hangs costs the round nothing, and its answer is
/// kept whenever it comes back (see [`latest_answer`]). Not while an ask for
/// the vault is still in flight in the same lane. When the ask was sent, if
/// it was.
fn ask(chain_id: u64, url: &str, vault: Address, lane: Lane) -> Option<std::time::Instant> {
    let executor = executor_for(url);
    let mut asks = executor.asks.lock().ok()?;
    if asks.in_flight.contains_key(&(chain_id, vault, lane)) {
        return None;
    }
    let sent = std::time::Instant::now();
    asks.in_flight.insert((chain_id, vault, lane), sent);
    tokio::spawn(ask_off_the_round(
        executor.clone(),
        url.to_string(),
        chain_id,
        vault,
        sent,
        lane,
    ));
    Some(sent)
}

/// When the vault's ask got no answer, or a 404 while the keeper still wants
/// its total (the executor hadn't taken the vault on yet, say), ask again: a
/// chain-minute after the round that first found it so, then after two, four
/// and so on up to [`MOST_SECONDS_BETWEEN_ASKS`], until an ask about the
/// vault is answered with a total. One answered with a total is not asked
/// again here, however slowly the answer came, so a slow executor is not
/// billed every minute.
fn ask_again(chain_id: u64, url: &str, vault: Address, lane: Lane, now: u64, memory: &mut Memory) {
    let key = (chain_id, vault);
    let unanswered = executor_for(url)
        .asks
        .lock()
        .is_ok_and(|asks| asks.unanswered.contains(&key) || asks.wanted_but_not_served(&key));
    if !unanswered {
        memory.ask_again = None;
        return;
    }
    match memory.ask_again {
        None => {
            memory.ask_again = Some(AskAgain {
                wait: MINUTE,
                next: now.saturating_add(MINUTE),
            });
        }
        Some(again) if now >= again.next => {
            if ask(chain_id, url, vault, lane).is_some() {
                let wait = (again.wait * 2).min(MOST_SECONDS_BETWEEN_ASKS);
                memory.ask_again = Some(AskAgain {
                    wait,
                    next: now.saturating_add(wait),
                });
            }
        }
        Some(_) => {}
    }
}

/// Ask the executor for a vault's total, and keep the answer unless a later
/// ask's is already kept.
///
/// An executor that can't be asked (no connection, no answer in time, a
/// failure, something that isn't a total) is said at warn at once, then at
/// most once every ten minutes with how many asks went unanswered. The vault
/// is asked again (see [`ask_again`]), and the executor is in the keeper's
/// health once it has answered no ask for [`UNANSWERED_FOR`], until it
/// answers any. A failed ask sent before the keeper last said it no longer
/// wants the vault's total (a closed vault that went quiet meanwhile) is not
/// counted. A 404 is taken at its
/// word, as no total for the vault, but said at warn the first time for each
/// vault, and again the first time after the executor has served the vault a
/// total: an executor URL that is wrong answers 404 for every vault, and its
/// bills would go uncollected without a word.
async fn ask_off_the_round(
    executor: SharedExecutor,
    url: String,
    chain_id: u64,
    vault: Address,
    sent: std::time::Instant,
    lane: Lane,
) {
    let answer = match executor.lane(lane).acquire().await {
        Ok(_asking) => fetch_total(chain_id, &url, vault).await,
        Err(_) => Err("the keeper is stopping".to_string()),
    };
    let Ok(mut asks) = executor.asks.lock() else {
        return;
    };
    let key = (chain_id, vault);
    if asks.in_flight.get(&(chain_id, vault, lane)) == Some(&sent) {
        asks.in_flight.remove(&(chain_id, vault, lane));
    }
    match answer {
        Ok(asked) => {
            asks.unanswered.remove(&key);
            asks.failing_since = None;
            if asks.said.over() {
                tracing::info!(chain_id, "the executor answers again");
            }
            match &asked {
                Asked::NotServed => {
                    if asks.not_served.insert(key) {
                        tracing::warn!(
                            chain_id,
                            %vault,
                            "the executor serves no total for this vault; if it should, check the executor's URL"
                        );
                    }
                }
                Asked::Total(_) => {
                    asks.not_served.remove(&key);
                }
            }
            if asks
                .latest
                .get(&key)
                .is_none_or(|(_, asked_at)| *asked_at < sent)
            {
                asks.latest.insert(key, (asked, sent));
            }
            asks.note_serving();
        }
        Err(error) => {
            if asks
                .not_wanted
                .get(&key)
                .is_some_and(|since| sent <= *since)
            {
                return;
            }
            asks.unanswered.insert(key);
            let now = std::time::Instant::now();
            asks.failing_since.get_or_insert(now);
            if let Some(times) = asks.said.due(now) {
                tracing::warn!(
                    chain_id,
                    %vault,
                    times,
                    error,
                    "the executor could not be asked for the vault's total; its bill waits for the next ask"
                );
            }
        }
    }
}

/// The latest answer the executor gave for a vault, and when the ask it
/// answered was sent, without asking it.
fn latest_answer(url: &str, chain_id: u64, vault: Address) -> Option<(Asked, std::time::Instant)> {
    let executor = executor_for(url);
    let asks = executor.asks.lock().ok()?;
    asks.latest.get(&(chain_id, vault)).cloned()
}

/// Wait for the vault's ask still in flight in `lane` to come back, or for
/// an answer to an ask sent no earlier than it, for at most `limit`. An ask
/// in the other lane isn't waited for: an older one says nothing newer.
async fn answer_in_flight(url: &str, chain_id: u64, vault: Address, lane: Lane, limit: Duration) {
    let executor = executor_for(url);
    let started = std::time::Instant::now();
    let waiting = |asks: &Asks| {
        asks.in_flight
            .get(&(chain_id, vault, lane))
            .is_some_and(|sent| {
                asks.latest
                    .get(&(chain_id, vault))
                    .is_none_or(|(_, answered)| answered < sent)
            })
    };
    while started.elapsed() < limit && executor.asks.lock().is_ok_and(|asks| waiting(&asks)) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The keeper no longer wants a total for the vault: it is no longer asked
/// again, nor counts against the executor in the keeper's health, and an ask
/// already sent that comes back with no answer doesn't put it back.
fn total_not_wanted(url: &str, chain_id: u64, vault: Address) {
    if let Ok(mut asks) = executor_for(url).asks.lock() {
        let key = (chain_id, vault);
        asks.unanswered.remove(&key);
        asks.not_wanted.insert(key, std::time::Instant::now());
        if asks.unanswered.is_empty() {
            asks.failing_since = None;
        }
        asks.note_serving();
    }
}

/// Every vault, by its chain id, whose latest ask its executor gave no
/// answer to, of each executor that has answered no ask for
/// [`UNANSWERED_FOR`].
fn unanswered_asks(now: std::time::Instant) -> Vec<(u64, Address)> {
    let Ok(executors) = EXECUTORS.lock() else {
        return Vec::new();
    };
    executors
        .values()
        .flat_map(|executor| {
            executor
                .asks
                .lock()
                .map(|asks| {
                    let failing = asks.failing_since.is_some_and(|since| {
                        now.saturating_duration_since(since) >= UNANSWERED_FOR
                    });
                    if failing {
                        asks.unanswered.iter().copied().collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_default()
        })
        .collect()
}

/// Whether an executor's latest answer for every vault the keeper still
/// wants a total for has been a 404, with none served a total, for
/// [`serving_none_for`]: a URL that is wrong (another host, the API instead
/// of the executor) answers so, and no bill is collected. A 404 for one vault
/// may be right (another executor serves it); once any wanted vault gets a
/// total, or no vault wants one, it isn't said. A vault closed and settled,
/// or no longer listed, doesn't count.
fn serving_none(asks: &Asks, now: std::time::Instant) -> bool {
    asks.serving_none_since
        .is_some_and(|since| now.saturating_duration_since(since) >= serving_none_for())
}

/// Whether any executor the keeper asks serves no vault (see
/// [`serving_none`]).
fn executors_serving_none(now: std::time::Instant) -> bool {
    let Ok(executors) = EXECUTORS.lock() else {
        return false;
    };
    executors.values().any(|executor| {
        executor
            .asks
            .lock()
            .is_ok_and(|asks| serving_none(&asks, now))
    })
}

/// Say at warn that the executor serves no vault (see [`serving_none`]), at
/// once, then at most every ten minutes with a count, while it doesn't.
fn say_serving_none(url: &str) {
    let executor = executor_for(url);
    let Ok(mut asks) = executor.asks.lock() else {
        return;
    };
    let now = std::time::Instant::now();
    if serving_none(&asks, now) {
        if let Some(times) = asks.serving_none_said.due(now) {
            tracing::warn!(
                times,
                "the executor has answered 404 for every vault it was asked about, and served none; check the executor's URL"
            );
        }
    } else if asks.serving_none_said.over() {
        tracing::info!("the executor serves a vault again");
    }
}

/// What the executor answered, or why it gave no answer.
async fn fetch_total(chain_id: u64, executor: &str, vault: Address) -> Result<Asked, String> {
    let reply = HTTP
        .get(format!("{executor}/usage/{chain_id}/{vault:?}"))
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    if reply.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Asked::NotServed);
    }
    if !reply.status().is_success() {
        return Err(format!("answered {}", reply.status()));
    }
    reply
        .json()
        .await
        .map(Asked::Total)
        .map_err(|error| format!("not a total: {}", error.without_url()))
}

/// What became of a call the keeper sent.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    /// It was mined and went through.
    Done,
    /// It was mined and reverted.
    Reverted,
    /// The node answered and refused it before it went out: its gas
    /// estimate reverted.
    Refused,
    /// Refused before it went out as the vault's `Stale`: a total signed
    /// later than the latest block, which a node behind the chain also
    /// says. Not counted as the vault refusing it; tried again next round.
    Stale,
    /// It never went out, or no answer came back: the node couldn't be
    /// reached, didn't answer in time, or turned it away for a reason of its
    /// own (the keeper's account can't pay for gas, a rate limit, a fee too
    /// low), which says nothing about whether the call would go through.
    NotSent,
}

impl Outcome {
    /// Whether it reached the chain at all.
    fn mined(self) -> bool {
        matches!(self, Outcome::Done | Outcome::Reverted)
    }
}

/// A call the keeper sends, as its not going out is kept: the chain's node,
/// the contract, what the call is for, the hash of the call itself, so two
/// calls for the same purpose (two keys' revokes at one station) are two,
/// and the function it calls.
type Call = (String, Address, String, B256, [u8; 4]);

/// Send one call as the keeper, and say what became of it. A call that can't
/// go through yet is ordinary: somebody else got there first, or it isn't
/// time.
///
/// One that doesn't go out is kept in the keeper's health (see
/// [`Failures`]) and said at warn: at once the first time the node turns a
/// send away for that reason, then at most once every ten minutes with how
/// many sends it has turned away for it since, however many calls fail.
async fn send_outcome<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
    what: &str,
) -> Outcome {
    send_outcome_with(client, keeper, to, call, what, false).await
}

/// [`send_outcome`], with the gas limit [`roomy_gas`] gives: for a call that
/// tries what it may let fail inside (see [`send_roomy`]).
async fn send_roomy_outcome<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
    what: &str,
) -> Outcome {
    send_outcome_with(client, keeper, to, call, what, true).await
}

async fn send_outcome_with<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
    what: &str,
    roomy: bool,
) -> Outcome {
    let sent: Call = (
        client.rpc_url.clone(),
        to,
        what.to_string(),
        alloy::primitives::keccak256(call.abi_encode()),
        C::SELECTOR,
    );
    // The vault holds one collected total at a time and takes none older, so
    // a `keep` sent now is the only one the round still wants: an earlier one
    // that didn't go out is taken off at the round's end.
    if C::SELECTOR == <IVault::keepCall as alloy::sol_types::SolCall>::SELECTOR
        && let Ok(mut not_sent) = NOT_SENT.lock()
    {
        not_sent.no_longer_wanted(|key| {
            key.0 == sent.0 && key.1 == sent.1 && key.4 == sent.4 && key.3 != sent.3
        });
    }
    // Whatever the node made of it, a send that reached it went out.
    let went_out = || {
        if let Ok(mut not_sent) = NOT_SENT.lock() {
            not_sent.went_out(&sent);
        }
    };
    let sending = if roomy {
        send_roomy(client, keeper, to, call).await
    } else {
        client.send(keeper, to, call).await
    };
    match sending.context(what.to_string()) {
        Err(error) => {
            tracing::debug!(%to, error = format!("{error:#}"), "not yet");
            if error.chain().any(|cause| cause.is::<chain::Reverted>()) {
                went_out();
                Outcome::Reverted
            } else if execution_reverted(&error) {
                went_out();
                if reverted_as_stale(&error) {
                    Outcome::Stale
                } else {
                    Outcome::Refused
                }
            } else {
                // Not the vault's answer but the node's, or none at all: a
                // keeper out of gas money, rate limited or underpriced does
                // nothing for anyone until somebody hears of it.
                let now = std::time::Instant::now();
                if let Ok(mut not_sent) = NOT_SENT.lock() {
                    not_sent.failed(sent.clone(), now);
                }
                let condition = (client.rpc_url.clone(), turned_away_for(&error));
                let due = TURNED_AWAY
                    .lock()
                    .ok()
                    .and_then(|mut turned| turned.entry(condition).or_default().due(now));
                if let Some(times) = due {
                    tracing::warn!(
                        chain_id = client.chain_id,
                        %to,
                        what,
                        times,
                        error = format!("{error:#}"),
                        "the keeper's send did not go out"
                    );
                }
                Outcome::NotSent
            }
        }
        Ok(_) => {
            tracing::debug!(%to, what, "done");
            went_out();
            Outcome::Done
        }
    }
}

/// Why the node turned a send away, as the keeper's warnings are kept: the
/// node's reason without the figures that differ from one send to the next
/// (a balance, a fee, an address or a hash, in digits or in hex), or that no
/// answer came at all. A keeper out of gas money is one condition, however
/// many of its calls fail for it, and the conditions kept stay few.
fn turned_away_for(error: &anyhow::Error) -> String {
    let said = error
        .chain()
        .find_map(|cause| {
            cause
                .downcast_ref::<alloy::transports::TransportError>()
                .and_then(|error| error.as_error_resp())
                .map(|refusal| refusal.message.to_string())
        })
        .unwrap_or_else(|| "no answer from the node".to_string());
    said.split(':')
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .split_whitespace()
        .filter(|word| {
            !word
                .trim_start_matches(|character: char| !character.is_ascii_alphanumeric())
                .starts_with("0x")
        })
        .map(|word| {
            word.chars()
                .filter(|character| !character.is_ascii_digit())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How long of the chain's time something the keeper no longer tries is
/// still said to be failing: longer than it waits between tries of anything
/// it still wants (a slot's valuation, a claim at its longest wait, a bill
/// once a minute), the daily round's apart.
const HELD_FOR: u64 = 15 * MINUTE;

/// How long something is said to be failing at most, should its chain's
/// rounds stop sweeping it.
const FORGET_AFTER: Duration = Duration::from_secs(DAY + 60 * MINUTE);

/// What has lately failed, so a keeper that can't send is heard of in its
/// health. Something is taken off only when the same call (the same
/// contract and bytes, on the same node, whatever it was sent for) next goes
/// out (mined, reverted, or refused on its merits), or at
/// the end of a round of its chain that no longer wants it: one not tried
/// for [`HELD_FOR`] of chain time, or one a later call has replaced. A round
/// on a clock that runs fast (the devnet moves an hour a block) takes it off
/// at the next round.
struct Failures<K>(HashMap<K, Failing>);

impl<K> Default for Failures<K> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

struct Failing {
    failed_at: std::time::Instant,
    /// The chain's time at the end of the round it last failed in, once a
    /// round has ended since.
    tried_at: Option<u64>,
    /// Whether a later call has replaced it: the next round's end takes it
    /// off.
    replaced: bool,
}

impl<K: Eq + std::hash::Hash> Failures<K> {
    fn failed(&mut self, key: K, now: std::time::Instant) {
        self.forget(now);
        self.0.insert(
            key,
            Failing {
                failed_at: now,
                tried_at: None,
                replaced: false,
            },
        );
    }

    /// At the end of a round on a chain whose clock reads `now`: what `mine`
    /// picks out of that chain's and the rounds no longer want (not tried for
    /// [`HELD_FOR`], or replaced) is taken off.
    fn sweep(&mut self, mine: impl Fn(&K) -> bool, now: u64) {
        self.0.retain(|key, failing| {
            if !mine(key) {
                return true;
            }
            let tried_at = *failing.tried_at.get_or_insert(now);
            !failing.replaced && now < tried_at.saturating_add(HELD_FOR)
        });
    }

    /// What `which` picks out is no longer wanted: a later call replaces it.
    /// It is taken off at the end of the round.
    fn no_longer_wanted(&mut self, which: impl Fn(&K) -> bool) {
        for (_, failing) in self.0.iter_mut().filter(|(key, _)| which(key)) {
            failing.replaced = true;
        }
    }

    /// Take off what hasn't failed for [`FORGET_AFTER`].
    fn forget(&mut self, now: std::time::Instant) {
        self.0
            .retain(|_, failing| now.duration_since(failing.failed_at) < FORGET_AFTER);
    }
}

impl Failures<Call> {
    /// The call reached the node: it is taken off under whatever it was
    /// sent for, since the same call (the same contract and bytes, on the same
    /// node) can be sent for two purposes in one round, and either going out
    /// is the call going out.
    fn went_out(&mut self, sent: &Call) {
        self.0
            .retain(|key, _| !(key.0 == sent.0 && key.1 == sent.1 && key.3 == sent.3));
    }

    /// What the calls are for, each once.
    fn whats(&self) -> String {
        let mut whats: Vec<&str> = self
            .0
            .keys()
            .map(|(_, _, what, ..)| what.as_str())
            .collect();
        whats.sort_unstable();
        whats.dedup();
        whats.join(", ")
    }
}

/// Calls that have lately not gone out.
static NOT_SENT: std::sync::LazyLock<std::sync::Mutex<Failures<Call>>> =
    std::sync::LazyLock::new(Default::default);

/// Vaults whose round has not finished, by their chain id, with how many
/// rounds in a row: a vault that can't be read at all is never valued,
/// settled or billed.
static UNREAD: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, Address), u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Rounds of a vault in a row that may fail before it is said not to be kept:
/// one that fails now and then is a node's hiccup. As many reads of its tank
/// may fail before it is said to be unreadable.
const MOST_VAULT_ROUNDS_FAILING: u64 = 3;

/// Open or halted vaults, by their chain id, whose tank couldn't be read in
/// at least [`MOST_VAULT_ROUNDS_FAILING`] rounds in a row, with how many (see
/// [`tank_read`]).
static TANKS_UNREAD: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, Address), u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Vaults, by their chain id, whose simulation of what the stopped
/// station's unwind leaves has failed at least [`MOST_PROBES_FAILING`] times
/// in a row, with how many: their unwind is not sent until it works.
static UNPROBED: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, Address), u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Vaults, by their chain id, that refuse the `keep` carrying their bill,
/// with how many rounds in a row: a bill never collected breaks the daily
/// bound on what is owed.
static BILLS_REFUSED: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, Address), u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Vaults, by their chain id, that have refused a valuation, or a claim's
/// try, before it went out, in at least [`MOST_SLOTS_REFUSED`] slots in a row,
/// with how many.
static CALLS_REFUSED: std::sync::LazyLock<std::sync::Mutex<CallsRefused>> =
    std::sync::LazyLock::new(Default::default);

/// Vaults, by their chain id, and what they keep refusing, with in how many
/// slots in a row.
type CallsRefused = HashMap<(u64, Address, Refused), u64>;

/// The reasons nodes have turned the keeper's sends away for, by node, as
/// said.
static TURNED_AWAY: std::sync::LazyLock<std::sync::Mutex<HashMap<(String, String), Said>>> =
    std::sync::LazyLock::new(Default::default);

/// The longest first line of the keeper's health: what a dashboard's row
/// shows.
const HEALTH_LINE: usize = 60;

/// What the keeper's health reads.
struct Troubles<'a> {
    not_sent: &'a Failures<Call>,
    /// Vaults whose latest ask of the executor went unanswered.
    unanswered: &'a [(u64, Address)],
    chains: &'a std::collections::BTreeMap<Chain, Kept>,
    /// Vaults whose rounds have not finished, and how many in a row.
    unread: &'a HashMap<(u64, Address), u64>,
    /// Vaults whose unwind's simulation keeps failing, and how many times in
    /// a row.
    unprobed: &'a HashMap<(u64, Address), u64>,
    /// Vaults that refuse the `keep` carrying their bill, and how many rounds
    /// in a row.
    refused: &'a HashMap<(u64, Address), u64>,
    /// Vaults that keep refusing valuations or claims' tries, and in how
    /// many slots in a row.
    calls_refused: &'a CallsRefused,
    /// Vaults whose tank couldn't be read, and how many rounds in a row.
    tanks_unread: &'a HashMap<(u64, Address), u64>,
    /// Whether an executor has answered 404 for every vault it was asked
    /// about, and served none, for [`SERVING_NONE_FOR`].
    serving_none: bool,
    /// What is wrong on the Solana vault chains, each as a short phrase and
    /// in full (see [`solana::troubles`]).
    solana: &'a [(String, String)],
}

/// The keeper's health: whether all is well, and what to say. The first line
/// is short, for a dashboard's row, at most [`HEALTH_LINE`] characters: the
/// most serious trouble and how many more kinds there are; each is said in
/// full on the lines after it. A chain to be kept that hasn't connected, has
/// no keeper account, or whose rounds aren't finishing (see [`Kept`]) is not
/// well, nor is one an earlier factory's list couldn't be read on in its last
/// round, nor a vault whose rounds haven't finished
/// [`MOST_VAULT_ROUNDS_FAILING`] times in a row, whose tank couldn't be read
/// as many rounds in a row, whose unwind's simulation has failed
/// [`MOST_PROBES_FAILING`] times in a row, that refuses the `keep` carrying
/// its bill, or that has refused its valuations or its claims' tries for
/// [`MOST_SLOTS_REFUSED`] slots in a row; nor an executor that doesn't answer
/// or serves no vault.
fn health(troubles: &Troubles, now: std::time::Instant) -> (bool, String) {
    let mut first = Vec::new();
    let mut details = Vec::new();
    let troubled: Vec<String> = troubles
        .chains
        .iter()
        .filter_map(|(chain, kept)| {
            kept.trouble(now)
                .map(|trouble| format!("{chain} ({trouble})"))
        })
        .collect();
    if !troubled.is_empty() {
        first.push(format!("{} chains not kept", troubled.len()));
        details.push(format!("not kept: {}", troubled.join(", ")));
    }
    let mut unread: Vec<String> = troubles
        .unread
        .iter()
        .filter(|(_, failing)| **failing >= MOST_VAULT_ROUNDS_FAILING)
        .map(|((chain_id, vault), failing)| {
            format!("{chain_id}/{vault:?} ({failing} rounds in a row)")
        })
        .collect();
    if !unread.is_empty() {
        unread.sort_unstable();
        first.push(format!("{} vaults not kept", unread.len()));
        details.push(format!("vault rounds not finishing: {}", unread.join(", ")));
    }
    let mut tanks_unread: Vec<String> = troubles
        .tanks_unread
        .iter()
        .map(|((chain_id, vault), rounds)| {
            format!("{chain_id}/{vault:?} ({rounds} rounds in a row)")
        })
        .collect();
    if !tanks_unread.is_empty() {
        tanks_unread.sort_unstable();
        first.push(format!("{} tanks unreadable", tanks_unread.len()));
        details.push(format!(
            "tanks that can't be read, so no bill is collected: {}",
            tanks_unread.join(", ")
        ));
    }
    let unlisted: Vec<String> = troubles
        .chains
        .iter()
        .filter(|(_, kept)| kept.factories_unread)
        .map(|(chain, _)| chain.to_string())
        .collect();
    if !unlisted.is_empty() {
        first.push("factory list unreadable".to_string());
        details.push(format!(
            "an earlier factory's vaults kept as last listed: {}",
            unlisted.join(", ")
        ));
    }
    let not_sent = troubles.not_sent;
    if !not_sent.0.is_empty() {
        first.push(format!("{} sends not going out", not_sent.0.len()));
        details.push(format!("not going out: {}", not_sent.whats()));
    }
    let mut unprobed: Vec<String> = troubles
        .unprobed
        .iter()
        .map(|((chain_id, vault), failing)| {
            format!("{chain_id}/{vault:?} ({failing} times in a row)")
        })
        .collect();
    if !unprobed.is_empty() {
        unprobed.sort_unstable();
        first.push(format!("{} unwinds not simulated", unprobed.len()));
        details.push(format!(
            "unwinds not sent, what they leave can't be simulated: {}",
            unprobed.join(", ")
        ));
    }
    let mut refused: Vec<String> = troubles
        .refused
        .iter()
        .map(|((chain_id, vault), rounds)| {
            format!("{chain_id}/{vault:?} ({rounds} rounds in a row)")
        })
        .collect();
    if !refused.is_empty() {
        refused.sort_unstable();
        first.push(format!("{} bills refused", refused.len()));
        details.push(format!(
            "bills the vault refuses to collect: {}",
            refused.join(", ")
        ));
    }
    for kind in [Refused::Valuations, Refused::Claims] {
        let mut vaults: Vec<String> = troubles
            .calls_refused
            .iter()
            .filter(|((.., refused), _)| *refused == kind)
            .map(|((chain_id, vault, _), slots)| {
                format!("{chain_id}/{vault:?} ({slots} slots in a row)")
            })
            .collect();
        if !vaults.is_empty() {
            vaults.sort_unstable();
            first.push(format!("{} {} refused", vaults.len(), kind.what()));
            details.push(format!(
                "{} the vault refuses: {}",
                kind.what(),
                vaults.join(", ")
            ));
        }
    }
    if !troubles.unanswered.is_empty() {
        first.push("executor not answering".to_string());
        let mut vaults: Vec<String> = troubles
            .unanswered
            .iter()
            .map(|(chain_id, vault)| format!("{chain_id}/{vault:?}"))
            .collect();
        vaults.sort_unstable();
        details.push(format!(
            "no total from the executor for: {}",
            vaults.join(", ")
        ));
    }
    if troubles.serving_none {
        first.push("executor serves no vault".to_string());
        details.push(
            "the executor answers 404 for every vault it is asked about: check its URL".to_string(),
        );
    }
    for (phrase, detail) in troubles.solana {
        first.push(format!("solana: {phrase}"));
        details.push(format!("solana: {detail}"));
    }
    let Some(worst) = first.first() else {
        return (true, "keeper".to_string());
    };
    let line = match first.len() {
        1 => format!("keeper: {worst}"),
        many => format!("keeper: {worst}; +{} more", many - 1),
    };
    details.insert(0, line.chars().take(HEALTH_LINE).collect());
    (false, details.join("\n"))
}

/// Whether the node answered that the call itself reverts, as its gas
/// estimate does when the contract refuses it. Nodes say so with code 3 or in
/// those words. Only that is the contract's answer: any other error the node
/// gives is its own, and the call may go through once that passes.
fn execution_reverted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<alloy::transports::TransportError>()
            .and_then(|error| error.as_error_resp())
            .is_some_and(|refusal| {
                refusal.code == 3
                    || refusal
                        .message
                        .to_lowercase()
                        .contains("execution reverted")
            })
    })
}

/// Whether the node's refusal names the vault's `Stale`: by its selector in
/// the revert data, or by name in the message, as some nodes give it.
fn reverted_as_stale(error: &anyhow::Error) -> bool {
    use alloy::sol_types::SolError;
    reverted_with(error, IVault::Stale::SELECTOR, "Stale()")
}

/// Whether the node's refusal names the error with this selector and
/// signature: by the selector in the revert data, or by name in the message.
fn reverted_with(error: &anyhow::Error, selector: [u8; 4], signature: &str) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<alloy::transports::TransportError>()
            .and_then(|error| error.as_error_resp())
            .is_some_and(|refusal| {
                refusal
                    .as_revert_data()
                    .is_some_and(|data| data.starts_with(&selector))
                    || refusal.message.contains(signature)
            })
    })
}

/// How much gas past one and a half times the estimate an unwind or the
/// Closer is sent with.
const ROOMY_GAS_MARGIN: u64 = 250_000;

/// The gas limit for a call that tries what it may let fail inside: a
/// stopped station's unwind sells each driver's holdings inside try/catch,
/// and the Closer makes that unwind. A node's estimate is the least gas with
/// which the call goes through, and a sale that runs out of gas is caught
/// rather than failing the call, so with only the estimate (an inner call
/// gets 63/64 of what is left) a sale can be swallowed and the station left
/// unemptied. Half again the estimate and a margin besides, held under the
/// block's gas limit when it is known, and never below the estimate.
///
/// This assumes the latest block's gas limit is the limit the send will be
/// held to, and that one block's limit holds for every block. HyperEVM's
/// small and big blocks break that: its small blocks hold about 2M gas, so
/// an unwind or Closer call estimated above about 1.17M gas is lifted past
/// the small-block limit here, and whether it then lands at all depends on the
/// chain's rules for big blocks.
fn roomy_gas(estimate: u64, block_limit: Option<u64>) -> u64 {
    let roomy = estimate
        .saturating_add(estimate / 2)
        .saturating_add(ROOMY_GAS_MARGIN);
    match block_limit {
        Some(limit) if limit > 0 => roomy.min(limit).max(estimate),
        _ => roomy,
    }
}

/// Send a call as the keeper with the gas limit [`roomy_gas`] gives from the
/// node's estimate, and wait for it to land.
async fn send_roomy<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
) -> Result<alloy::rpc::types::TransactionReceipt> {
    use alloy::network::TransactionBuilder;
    use alloy::providers::Provider;
    let request = alloy::rpc::types::TransactionRequest::default()
        .with_from(keeper)
        .with_to(to)
        .with_input(call.abi_encode());
    let estimate = client
        .provider()
        .estimate_gas(request)
        .await
        .with_context(|| format!("estimating the gas of {}", C::SIGNATURE))?;
    let block_limit = client
        .provider()
        .get_block(alloy::eips::BlockId::latest())
        .await
        .ok()
        .flatten()
        .map(|block| block.header.gas_limit);
    client
        .send_with_gas_limit(keeper, to, call, roomy_gas(estimate, block_limit))
        .await
}

/// Send one call as the keeper, and say whether it went through.
async fn send<C: alloy::sol_types::SolCall>(
    client: &Client,
    keeper: Address,
    to: Address,
    call: C,
    what: &str,
) -> bool {
    send_outcome(client, keeper, to, call, what).await == Outcome::Done
}

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(|| async {
            let now = std::time::Instant::now();
            // Read before the rest is locked: the asks lock their own.
            let unanswered = unanswered_asks(now);
            let serving_none = executors_serving_none(now);
            let solana = solana::troubles(now);
            let (
                Ok(mut not_sent),
                Ok(chains),
                Ok(unread),
                Ok(unprobed),
                Ok(refused),
                Ok(calls_refused),
                Ok(tanks_unread),
            ) = (
                NOT_SENT.lock(),
                CHAINS.lock(),
                UNREAD.lock(),
                UNPROBED.lock(),
                BILLS_REFUSED.lock(),
                CALLS_REFUSED.lock(),
                TANKS_UNREAD.lock(),
            )
            else {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "keeper: health unreadable".to_string(),
                );
            };
            not_sent.forget(now);
            let troubles = Troubles {
                not_sent: &not_sent,
                unanswered: &unanswered,
                chains: &chains,
                unread: &unread,
                unprobed: &unprobed,
                refused: &refused,
                calls_refused: &calls_refused,
                tanks_unread: &tanks_unread,
                serving_none,
                solana: &solana,
            };
            // Anything failing is not "up": a dashboard or a supervisor that
            // reads only the status still hears of it.
            match health(&troubles, now) {
                (true, said) => (axum::http::StatusCode::OK, said),
                (false, said) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, said),
            }
        }),
    );
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}

mod attest;
mod delayed;
mod solana;

#[cfg(test)]
mod stand_in;

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use alloy::primitives::B256;
    use alloy::sol_types::SolCall;
    use chain::bindings::station::StationSpec;
    use chain::bindings::vault::{Claim, Valuation};

    #[test]
    fn units_a_staged_close_holds_off_are_never_frozen_for_meanwhile() {
        let week = 7 * 24 * 3600;
        // A week unserved, nothing staged: due.
        assert!(units_freeze_due(0, 1_000 + week, 1_000, week, false));
        // The same while a close is staged: held off, as a claim is.
        assert!(!units_freeze_due(0, 1_000 + week, 1_000, week, true));
        // Not yet a week, or the vault halted: never.
        assert!(!units_freeze_due(0, 999 + week, 1_000, week, false));
        assert!(!units_freeze_due(5, 1_000 + week, 1_000, week, false));
    }

    use crate::stand_in::{
        BETWEEN_SLOTS, Failing, Hook, KEEPER, SLOT_START, STATION, Shared, VAULT, answers, connect,
        connect_failing, connect_with_blocks_of, connect_with_hook, deployments, everything_sent,
        executor, executor_serving_nothing, failing_executor, keeps_the_window,
        leaves_after_unwind, name, node, refuses, reverts, sent, sent_at, simulated_at, takes,
        timers,
    };

    #[test]
    fn the_daily_keep_without_a_total_is_sent_only_to_queue_a_fill() {
        // Only a fill the author wants and none queued yet.
        assert!(daily(true, false, true).keep);
        assert!(!daily(true, false, false).keep);
        // A halted vault is stopping: its tank needs no more.
        assert!(!daily(false, true, true).keep);
    }

    #[test]
    fn the_restricted_list_is_read_again_only_while_the_vault_is_open() {
        assert!(daily(true, false, false).refresh);
        // Winding down or halted, nobody new can come in for it to keep out.
        assert!(!daily(false, false, true).refresh);
        assert!(!daily(true, true, true).refresh);
    }

    /// The keeper's health with nothing failing but what is given.
    fn health_with(
        not_sent: &Failures<Call>,
        unanswered: &[(u64, Address)],
        chains: &std::collections::BTreeMap<Chain, Kept>,
        now: std::time::Instant,
    ) -> (bool, String) {
        health(
            &Troubles {
                not_sent,
                unanswered,
                chains,
                unread: &HashMap::new(),
                unprobed: &HashMap::new(),
                refused: &HashMap::new(),
                calls_refused: &HashMap::new(),
                tanks_unread: &HashMap::new(),
                serving_none: false,
                solana: &[],
            },
            now,
        )
    }

    fn signed(total: u64, as_of: u64) -> Signed {
        Signed {
            total: total.to_string(),
            as_of,
            signature: Bytes::new(),
        }
    }

    fn a_tank(now: u64, settled: u64, last_collection: u64) -> Tank {
        Tank {
            balance: U256::from(50_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::from(settled),
            usageAsOf: now - 2 * DAY,
            lastCollection: last_collection,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        }
    }

    #[test]
    fn a_keep_is_known_to_charge_nothing_from_an_empty_tank_or_past_the_grace() {
        let now = BETWEEN_SLOTS;
        let full = a_tank(now, 0, now - DAY);
        assert_eq!(nothing_to_charge(&full, None), None);
        // A collection short of the grace's end still charges.
        assert_eq!(nothing_to_charge(&full, Some(now - DAY + 1)), None);
        assert_eq!(
            nothing_to_charge(&full, Some(now - DAY)),
            Some(Billing::Capped)
        );
        // Past the grace, a cap still carried can still be charged.
        let carried = Tank {
            capCarried: U256::from(3_000_000u64),
            ..full
        };
        assert_eq!(nothing_to_charge(&carried, Some(now - DAY)), None);
        let empty = Tank {
            balance: U256::ZERO,
            ..full
        };
        assert_eq!(nothing_to_charge(&empty, None), Some(Billing::EmptyTank));
    }

    #[test]
    fn a_signed_total_bills_only_what_the_vault_has_not_collected() {
        let now = BETWEEN_SLOTS;
        let tank = a_tank(now, 1_000_000, now - 2 * DAY);
        assert!(owes(&signed(1_500_000, now), &tank, 0));
        // No more than the vault has collected.
        assert!(!owes(&signed(1_000_000, now), &tank, 0));
        // Signed no later than the total the vault collected: refused.
        assert!(!owes(&signed(1_500_000, now - 2 * DAY), &tank, 0));
        // Already sent.
        assert!(!owes(&signed(1_500_000, now), &tank, now));
    }

    #[test]
    fn a_vault_is_valued_once_a_slot_when_the_valuation_will_land_in_its_window() {
        // Slots every 600 seconds, a 60-second window, blocks 12 seconds apart.
        assert!(due_for_valuation(1_200, 600, 60, 12, 600));
        assert!(due_for_valuation(1_248, 600, 60, 12, 600));
        // It would land after the window closes.
        assert!(!due_for_valuation(1_249, 600, 60, 12, 600));
        assert!(!due_for_valuation(1_261, 600, 60, 12, 600));
        // Already valued for this slot.
        assert!(!due_for_valuation(1_210, 600, 60, 12, 1_200));
        // Sent at the end of a slot, it lands in the next slot's window.
        assert!(due_for_valuation(1_790, 600, 60, 12, 1_200));
        assert!(!due_for_valuation(1_790, 600, 60, 12, 1_800));
    }

    #[test]
    fn a_send_that_must_land_in_the_window_is_judged_by_the_slot_it_lands_in() {
        // Slots every 10 seconds, a 3-second window.
        assert_eq!(landing_slot(1_000, 10, 3, 1), Some(1_000));
        assert_eq!(landing_slot(1_002, 10, 3, 1), Some(1_000));
        assert_eq!(landing_slot(1_003, 10, 3, 1), None);
        // Two-second blocks: one second in still lands at the window's end.
        assert_eq!(landing_slot(1_001, 10, 3, 2), Some(1_000));
        assert_eq!(landing_slot(1_002, 10, 3, 2), None);
        // Nine seconds in, it lands at the next slot's first second.
        assert_eq!(landing_slot(1_009, 10, 3, 2), Some(1_010));
        // The devnet: a block an hour on, every block on a slot boundary.
        assert_eq!(landing_slot(1_000, 10, 3, 3_600), Some(4_600));
        // A block interval that couldn't be measured sends nothing.
        assert_eq!(landing_slot(1_000, 10, 3, u64::MAX), None);
    }

    #[tokio::test]
    async fn log_reads_are_cut_into_bounded_ranges() {
        let node = node(BETWEEN_SLOTS);
        node.lock().unwrap().head = 25_004;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        find_holders(&client, VAULT, 5, &mut memory).await.unwrap();

        assert_eq!(
            node.lock().unwrap().log_reads,
            [(5, 10_004), (10_005, 20_004), (20_005, 25_004)]
        );
        assert_eq!(memory.holders_read_to, 25_005);
    }

    #[tokio::test]
    async fn a_log_range_the_node_refuses_is_halved_until_it_is_taken() {
        let node = node(BETWEEN_SLOTS);
        node.lock().unwrap().head = 25_004;
        node.lock().unwrap().most_log_blocks = Some(3_000);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        find_holders(&client, VAULT, 5, &mut memory).await.unwrap();

        let reads = node.lock().unwrap().log_reads.clone();
        assert_eq!(reads.first(), Some(&(5, 2_504)), "{reads:?}");
        assert!(
            reads.iter().all(|(from, to)| to - from < 3_000),
            "{reads:?}"
        );
        // Each read carries on from the last, with no gap.
        assert!(
            reads.windows(2).all(|pair| pair[1].0 == pair[0].1 + 1),
            "{reads:?}"
        );
        assert_eq!(memory.holders_read_to, reads.last().unwrap().1 + 1);
    }

    #[tokio::test]
    async fn a_log_range_the_node_answers_with_an_http_error_is_halved_until_it_is_taken() {
        let node = node(BETWEEN_SLOTS);
        node.lock().unwrap().head = 25_004;
        node.lock().unwrap().most_log_blocks_answered = Some(3_000);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        find_holders(&client, VAULT, 5, &mut memory).await.unwrap();

        let reads = node.lock().unwrap().log_reads.clone();
        assert_eq!(reads.first(), Some(&(5, 2_504)), "{reads:?}");
        assert!(
            reads.windows(2).all(|pair| pair[1].0 == pair[0].1 + 1),
            "{reads:?}"
        );
        assert_eq!(memory.holders_read_to, reads.last().unwrap().1 + 1);
    }

    #[tokio::test]
    async fn a_node_that_refuses_even_the_narrowest_range_leaves_the_reading_where_it_was() {
        let node = node(BETWEEN_SLOTS);
        node.lock().unwrap().head = 25_004;
        node.lock().unwrap().most_log_blocks = Some(0);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        assert!(find_holders(&client, VAULT, 5, &mut memory).await.is_err());
        assert_eq!(memory.holders_read_to, 0);
    }

    fn would(sent_home: u64, blocked: bool, mark: Option<u64>, mark_after: Option<u64>) -> Unwind {
        Unwind {
            sent_home: U256::from(sent_home),
            blocked,
            mark: mark.map(U256::from),
            mark_after: mark_after.map(U256::from),
            dust: U256::ZERO,
        }
    }

    #[test]
    fn an_unwind_is_sent_only_when_its_simulation_brings_home_a_cent_or_empties_the_station() {
        let cent = UNWIND_WORTH_AT_LEAST;
        // A cent coming home, whatever else.
        assert!(worth_sending(&would(cent, false, Some(cent), None)));
        assert!(worth_sending(&would(cent, true, None, None)));
        // Less than a cent coming home, and nothing to empty.
        assert!(!worth_sending(&would(
            cent - 1,
            false,
            Some(cent - 1),
            Some(0)
        )));
        // Nothing coming home, but the station emptied: a levered account
        // closed into its own debt.
        assert!(worth_sending(&would(0, false, Some(6 * cent), Some(0))));
        assert!(worth_sending(&would(
            0,
            false,
            Some(6 * cent),
            Some(cent - 1)
        )));
        // Nothing coming home and the station left as it was: a remnant the
        // venue values at the mark and never sells.
        assert!(!worth_sending(&would(
            0,
            false,
            Some(6 * cent),
            Some(6 * cent)
        )));
        // A cent's worth or more still left is not empty.
        assert!(!worth_sending(&would(0, false, Some(6 * cent), Some(cent))));
        // What it leaves unknown, or a market outside the band.
        assert!(!worth_sending(&would(0, false, Some(6 * cent), None)));
        assert!(!worth_sending(&would(0, true, Some(6 * cent), Some(0))));
        // No mark to go by: only what comes home counts.
        assert!(!worth_sending(&would(cent - 1, false, None, None)));
        // More than the dust threshold, both ways.
        let dusty = |sent_home: u64, mark: u64, after: u64| Unwind {
            dust: U256::from(2 * cent),
            ..would(sent_home, false, Some(mark), Some(after))
        };
        assert!(!worth_sending(&dusty(2 * cent, 2 * cent, 2 * cent)));
        assert!(worth_sending(&dusty(2 * cent + 1, 0, 0)));
        assert!(!worth_sending(&dusty(0, 2 * cent, 0)));
        assert!(worth_sending(&dusty(0, 2 * cent + 1, 2 * cent)));
    }

    /// The creation code that simulates an unwind and the value after it
    /// names the station in both calls, and each failed call jumps to its
    /// revert, which passes the call's own reason on.
    #[test]
    fn the_unwind_simulation_calls_the_station_and_reverts_with_its_reason() {
        let code = unwind_probe(STATION);
        let named = code
            .windows(21)
            .filter(|window| window[0] == 0x73 && window[1..] == STATION[..])
            .count();
        assert_eq!(named, 2);
        // PUSH2 <target> JUMPI, twice, to a JUMPDEST.
        let targets: Vec<usize> = code
            .windows(4)
            .filter(|window| window[0] == 0x61 && window[3] == 0x57)
            .map(|window| usize::from(u16::from_be_bytes([window[1], window[2]])))
            .collect();
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|target| code[*target] == 0x5b));
        assert_eq!(code.last(), Some(&0xfd));
    }

    #[test]
    fn an_unwind_not_worth_sending_is_simulated_again_ever_less_often_up_to_five_minutes() {
        let interval = 10;
        let remnant = would(0, false, Some(50_000), Some(50_000));
        let mut checks: Option<Backoff> = None;
        let mut waits = Vec::new();
        let mut slot = 0;
        for _ in 0..8 {
            let next = next_check(checks.as_ref(), &remnant, slot, interval);
            assert!(next.idle);
            waits.push(next.wait);
            slot = next.next;
            checks = Some(next);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        // A market outside the band may come back: not idle.
        assert!(!next_check(None, &would(0, true, Some(50_000), None), 0, interval).idle);
        // A slot longer than the longest wait waits a slot.
        assert_eq!(next_check(checks.as_ref(), &remnant, 0, 600).wait, 1);
    }

    #[test]
    fn a_condition_sends_fail_for_is_said_at_once_then_at_most_every_ten_minutes_with_a_count() {
        let mut said = Said::default();
        let start = std::time::Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);

        // The first time is said at once.
        assert_eq!(said.due(at(0)), Some(1));
        // Those in the next ten minutes are counted, not said.
        for seconds in 1..=5 {
            assert_eq!(said.due(at(seconds * 60)), None);
        }
        // Ten minutes on, it is said again with how many there have been.
        assert_eq!(said.due(at(600)), Some(6));
        assert_eq!(said.due(at(601)), None);
        // Once it is over, its next time is said at once.
        assert!(said.over());
        assert_eq!(said.due(at(602)), Some(1));
    }

    #[test]
    fn a_call_that_does_not_go_out_is_kept_by_the_call_itself_until_it_goes_out() {
        let mut not_sent = Failures::<Call>::default();
        let call = |what: &str, data: &[u8]| {
            (
                "node".to_string(),
                STATION,
                what.to_string(),
                alloy::primitives::keccak256(data),
                [0; 4],
            )
        };
        let revoke_a = call("revoke a key that no longer stands", b"a");
        let revoke_b = call("revoke a key that no longer stands", b"b");
        let unwind = call("unwind the station", b"unwind");
        let now = std::time::Instant::now();
        let none = [];
        let chains = std::collections::BTreeMap::new();
        assert_eq!(
            health_with(&not_sent, &none, &chains, now),
            (true, "keeper".to_string())
        );

        not_sent.failed(revoke_a.clone(), now);
        not_sent.failed(unwind.clone(), now);
        // Another call for the same purpose at the same contract going out
        // takes nothing off.
        not_sent.went_out(&revoke_b);
        assert_eq!(
            health_with(&not_sent, &none, &chains, now),
            (
                false,
                "keeper: 2 sends not going out\nnot going out: revoke a key that no longer stands, unwind the station"
                    .to_string()
            )
        );
        not_sent.went_out(&revoke_a);
        not_sent.went_out(&unwind);
        assert_eq!(
            health_with(&not_sent, &none, &chains, now),
            (true, "keeper".to_string())
        );
    }

    /// A call is taken off only when it goes out itself, or at the end of a
    /// round that no longer wants it. One claim's try going through takes
    /// nothing off another claim's that is still not going out, though the
    /// other was not tried again this round (it waits its turn). A bill
    /// turned away with one total and then sent with a newer one is no
    /// longer wanted: it is taken off at the end of the round, whatever
    /// became of the newer one.
    #[test]
    fn a_call_is_taken_off_only_by_going_out_itself_or_by_a_round_that_no_longer_wants_it() {
        let mut not_sent = Failures::<Call>::default();
        let call = |what: &str, data: &[u8], selector: [u8; 4]| {
            (
                "node".to_string(),
                VAULT,
                what.to_string(),
                alloy::primitives::keccak256(data),
                selector,
            )
        };
        let mine = |(rpc, ..): &Call| rpc == "node";
        let now = std::time::Instant::now();
        let keep = IVault::keepCall::SELECTOR;
        let keep_t1 = call("collect the last bill", b"t1", keep);
        let keep_t2 = call("collect the last bill", b"t2", keep);
        let claim_a = call("serve a claim", b"a", [1; 4]);
        let claim_b = call("serve a claim", b"b", [1; 4]);

        // One round: the first bill and claim A don't go out.
        not_sent.failed(keep_t1.clone(), now);
        not_sent.failed(claim_a.clone(), now);
        not_sent.sweep(mine, 1_000);
        // The next: claim A waits its turn, claim B goes out, and a bill with
        // a newer total is sent.
        not_sent.went_out(&claim_b);
        not_sent.no_longer_wanted(|key| key.4 == keep && *key != keep_t2);
        assert!(not_sent.0.contains_key(&claim_a));
        assert!(
            not_sent.0.contains_key(&keep_t1),
            "taken off before the round's end"
        );
        not_sent.sweep(mine, 1_010);

        let left: Vec<&Call> = not_sent.0.keys().collect();
        assert_eq!(left, [&claim_a]);
    }

    /// Every kind of trouble at once: the first line a dashboard shows still
    /// fits in 60 characters, naming the most serious and counting the
    /// rest, and every one is said in full after it.
    #[test]
    fn the_first_line_of_the_health_fits_a_dashboard_s_row_whatever_is_wrong() {
        let now = std::time::Instant::now();
        let mut not_sent = Failures::<Call>::default();
        for n in 0..1_000u64 {
            not_sent.failed(
                (
                    "node".to_string(),
                    VAULT,
                    "serve a claim".to_string(),
                    alloy::primitives::keccak256(n.to_be_bytes()),
                    [0; 4],
                ),
                now,
            );
        }
        let mut chains = std::collections::BTreeMap::new();
        for chain in [Chain::Base, Chain::Hyperliquid, Chain::Robinhood] {
            chains.insert(chain, Kept::default());
        }
        chains.insert(
            Chain::Base,
            Kept {
                connected: Some(now),
                cannot_simulate: true,
                ..Kept::default()
            },
        );
        let unread = HashMap::from([((31337, VAULT), 12_345u64), ((31337, STATION), 1)]);
        let unprobed = HashMap::from([((31337, VAULT), 40u64)]);
        let refused = HashMap::from([((31337, VAULT), 7u64)]);
        let calls_refused = HashMap::from([((31337, VAULT, Refused::Valuations), 10u64)]);
        let (up, said) = health(
            &Troubles {
                not_sent: &not_sent,
                unanswered: &[(31337, VAULT)],
                chains: &chains,
                unread: &unread,
                unprobed: &unprobed,
                refused: &refused,
                calls_refused: &calls_refused,
                tanks_unread: &HashMap::new(),
                serving_none: false,
                solana: &[],
            },
            now,
        );
        assert!(!up);
        let first = said.lines().next().unwrap_or_default();
        assert_eq!(first, "keeper: 3 chains not kept; +6 more", "{said}");
        for told in [
            "not kept: base (can't simulate), hyperliquid (not connected)",
            &format!("vault rounds not finishing: 31337/{VAULT:?} (12345 rounds in a row)"),
            "not going out: serve a claim",
            &format!(
                "unwinds not sent, what they leave can't be simulated: 31337/{VAULT:?} (40 times in a row)"
            ),
            &format!("bills the vault refuses to collect: 31337/{VAULT:?} (7 rounds in a row)"),
            &format!("valuations the vault refuses: 31337/{VAULT:?} (10 slots in a row)"),
            &format!("no total from the executor for: 31337/{VAULT:?}"),
        ] {
            assert!(said.contains(told), "{told:?} not in {said}");
        }
        // A vault whose round failed once is a node's hiccup.
        assert!(!said.contains(&format!("{STATION:?}")), "{said}");

        // One kind alone is said as it is.
        let (_, said) = health_with(&not_sent, &[], &Default::default(), now);
        assert_eq!(
            said.lines().next(),
            Some("keeper: 1000 sends not going out")
        );
    }

    /// A chain with hundreds of vaults takes minutes a round. It is not said
    /// to stall inside three of its own rounds, however long those are, nor
    /// a quick chain inside three minutes; past that, either is.
    #[test]
    fn a_chain_whose_rounds_are_long_is_not_said_to_stall_inside_three_of_them() {
        let start = std::time::Instant::now();
        let minutes = |n: u64| Duration::from_secs(n * MINUTE);
        let long = Kept {
            connected: Some(start),
            finished: Some(start),
            took: minutes(10),
            ..Kept::default()
        };
        assert_eq!(long.trouble(start + minutes(29)), None);
        assert_eq!(
            long.trouble(start + minutes(31)),
            Some("rounds not finishing")
        );
        let quick = Kept {
            took: Duration::from_secs(5),
            ..long
        };
        assert_eq!(quick.trouble(start + minutes(2)), None);
        assert_eq!(
            quick.trouble(start + minutes(4)),
            Some("rounds not finishing")
        );
    }

    /// A call the keeper no longer makes (a cancel somebody else has since
    /// sent, so the keeper finds nothing to do) is not said to be failing for
    /// long: at the end of a round, one its chain's rounds have not tried
    /// for fifteen minutes of the chain's time is taken off. Another chain's
    /// round takes off nothing of this one's.
    #[test]
    fn a_call_the_rounds_no_longer_try_is_no_longer_said_to_fail() {
        let mut not_sent = Failures::<Call>::default();
        let start = std::time::Instant::now();
        let cancel = (
            "node".to_string(),
            VAULT,
            "cancel".to_string(),
            B256::ZERO,
            IVault::cancelCall::SELECTOR,
        );
        let mine = |(rpc, ..): &Call| rpc == "node";
        let theirs = |(rpc, ..): &Call| rpc == "another node";
        not_sent.failed(cancel.clone(), start);
        // The round it failed in ends.
        not_sent.sweep(mine, 1_000);
        not_sent.sweep(theirs, 1_000 + DAY);
        not_sent.sweep(mine, 1_000 + HELD_FOR - 1);
        assert!(not_sent.0.contains_key(&cancel));
        not_sent.sweep(mine, 1_000 + HELD_FOR);
        assert!(not_sent.0.is_empty());

        // One tried again in the meantime is held on from that try.
        not_sent.failed(cancel.clone(), start);
        not_sent.sweep(mine, 1_000);
        not_sent.failed(cancel.clone(), start);
        not_sent.sweep(mine, 1_000 + HELD_FOR - 1);
        not_sent.sweep(mine, 1_000 + HELD_FOR);
        assert!(not_sent.0.contains_key(&cancel));

        // Should no round sweep it, it is forgotten in the end.
        not_sent.forget(start + FORGET_AFTER);
        assert!(not_sent.0.is_empty());
    }

    /// An executor whose ask about one vault went unanswered is no longer
    /// said not to answer once it answers any ask. That vault is still asked
    /// again: only an answer about it says what it owes.
    #[tokio::test]
    async fn an_executor_that_answers_any_ask_is_no_longer_said_not_to_answer() {
        let executor = executor_serving_nothing().await;
        let other = Address::repeat_byte(0xa9);
        let now = std::time::Instant::now();
        {
            let shared = executor_for(&executor);
            let mut asks = shared.asks.lock().unwrap();
            asks.unanswered.insert((31337, other));
            asks.failing_since = now.checked_sub(UNANSWERED_FOR);
        }
        assert!(unanswered_asks(now).contains(&(31337, other)));

        ask(31337, &executor, VAULT, Lane::Daily);
        answer_in_flight(&executor, 31337, VAULT, Lane::Daily, Duration::from_secs(5)).await;

        assert!(!unanswered_asks(now).contains(&(31337, other)));
        assert!(unanswered(&executor, other));
    }

    /// An executor is said not to answer only once it has answered no ask
    /// for fifteen minutes: a restart, or one ask lost, is not a trouble.
    #[test]
    fn an_executor_is_said_not_to_answer_only_after_fifteen_minutes_of_no_answers() {
        let executor = "http://not-answering-for-a-while.invalid";
        let vault = Address::repeat_byte(0xb3);
        let now = std::time::Instant::now();
        let failing_for = |minutes: u64| {
            let shared = executor_for(executor);
            let mut asks = shared.asks.lock().unwrap();
            asks.unanswered.insert((31337, vault));
            asks.failing_since = now.checked_sub(Duration::from_secs(minutes * MINUTE));
        };
        failing_for(14);
        assert!(!unanswered_asks(now).contains(&(31337, vault)));
        failing_for(15);
        assert!(unanswered_asks(now).contains(&(31337, vault)));
    }

    /// The keeper no longer wanting a vault's total (its bill settled, its
    /// tank empty) takes it off what the executor is said not to answer.
    #[test]
    fn a_vault_whose_total_is_no_longer_wanted_is_not_said_to_go_unanswered() {
        let executor = "http://no-longer-wanted.invalid";
        let vault = Address::repeat_byte(0xb2);
        executor_for(executor)
            .asks
            .lock()
            .unwrap()
            .unanswered
            .insert((31337, vault));
        total_not_wanted(executor, 31337, vault);
        assert!(!unanswered(executor, vault));
        assert!(!unanswered_asks(std::time::Instant::now()).contains(&(31337, vault)));
    }

    /// An executor that answers every ask in three seconds: the ask doesn't
    /// hold the round up, another ask isn't sent while it is in flight, and
    /// its answer is kept when it comes back.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_executor_s_answer_is_kept_when_it_comes_back_and_holds_no_round_up() {
        let app = axum::Router::new().route(
            "/usage/{chain}/{vault}",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                axum::Json(serde_json::json!({
                    "total": "1000",
                    "as_of": 1_000,
                    "signature": "0x01",
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let executor = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let vault = Address::repeat_byte(0xb1);

        let started = std::time::Instant::now();
        assert!(ask(31337, &executor, vault, Lane::Daily).is_some());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(latest_answer(&executor, 31337, vault).is_none());
        assert!(
            ask(31337, &executor, vault, Lane::Daily).is_none(),
            "asked again while the first ask was in flight"
        );
        answer_in_flight(&executor, 31337, vault, Lane::Daily, Duration::from_secs(5)).await;
        assert!(matches!(
            latest_answer(&executor, 31337, vault),
            Some((Asked::Total(signed), _)) if signed.total == "1000"
        ));
    }

    /// The reasons a node turns sends away for are kept without what differs
    /// from one send to the next, in digits or in hex: one condition stays
    /// one, however many accounts, fees or hashes it names.
    #[test]
    fn a_reason_for_turning_sends_away_is_kept_without_its_figures_or_addresses() {
        let turned = |message: String| {
            let error: alloy::transports::TransportError =
                alloy::transports::TransportError::ErrorResp(alloy::rpc::json_rpc::ErrorPayload {
                    code: -32000,
                    message: message.into(),
                    data: None,
                });
            turned_away_for(&anyhow::Error::new(error))
        };
        let reasons: std::collections::BTreeSet<String> = (0..50u64)
            .flat_map(|n| {
                [
                    turned(format!(
                        "insufficient funds for gas * price + value: balance {n}"
                    )),
                    turned(format!(
                        "account {:?} is over its limit of {n} pending",
                        Address::repeat_byte(n as u8)
                    )),
                    turned(format!(
                        "known transaction ({:?})",
                        B256::repeat_byte(n as u8)
                    )),
                ]
            })
            .collect();
        assert_eq!(
            reasons.into_iter().collect::<Vec<_>>(),
            [
                "account is over its limit of pending",
                "insufficient funds for gas * price + value",
                "known transaction",
            ]
        );
    }

    /// The node turns a freeze away while the keeper can't pay for gas.
    /// Meanwhile a depositor freezes the vault themselves. Paid for again,
    /// the keeper's next freeze reaches the node and is refused on its
    /// merits: it went out, and is no longer said not to.
    #[tokio::test]
    async fn a_send_refused_on_its_merits_after_failing_to_go_out_is_no_longer_said_to_fail() {
        let to = Address::repeat_byte(0xa1);
        let node = node(BETWEEN_SLOTS);
        let turning_away = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_turning_away::<IVault::freezeCall>(
            &node,
            serde_json::json!({"code": -32000, "message": "insufficient funds for gas * price + value: balance 0"}),
            turning_away.clone(),
        )
        .await;
        let call = IVault::freezeCall {
            claimId: U256::from(1u64),
        };
        let key = (
            client.rpc_url.clone(),
            to,
            "freeze".to_string(),
            alloy::primitives::keccak256(call.abi_encode()),
            IVault::freezeCall::SELECTOR,
        );

        let first = send_outcome(&client, KEEPER, to, call.clone(), "freeze").await;
        assert_eq!(first, Outcome::NotSent);
        assert!(NOT_SENT.lock().unwrap().0.contains_key(&key));

        turning_away.store(false, Ordering::SeqCst);
        refuses(&node, to, call.clone());
        let second = send_outcome(&client, KEEPER, to, call, "freeze").await;
        assert_eq!(second, Outcome::Refused);
        assert!(
            !NOT_SENT.lock().unwrap().0.contains_key(&key),
            "the node answered the freeze on its merits, yet it is still said not to go out"
        );
    }

    /// The same, where the next send goes out, is mined and reverts
    /// (somebody's transaction landed first in the block): it went out.
    #[tokio::test]
    async fn a_send_that_went_out_and_reverted_is_no_longer_said_not_to_go_out() {
        let to = Address::repeat_byte(0xa2);
        let node = node(BETWEEN_SLOTS);
        let reverting = Arc::new(AtomicBool::new(false));
        let client =
            crate::stand_in::connect_turning_away_then_reverting(&node, reverting.clone()).await;
        let key = (
            client.rpc_url.clone(),
            to,
            "close".to_string(),
            alloy::primitives::keccak256(IVault::closeCall {}.abi_encode()),
            IVault::closeCall::SELECTOR,
        );

        assert_eq!(
            send_outcome(&client, KEEPER, to, IVault::closeCall {}, "close").await,
            Outcome::NotSent
        );
        assert!(NOT_SENT.lock().unwrap().0.contains_key(&key));

        reverting.store(true, Ordering::SeqCst);
        assert_eq!(
            send_outcome(&client, KEEPER, to, IVault::closeCall {}, "close").await,
            Outcome::Reverted
        );
        assert!(
            !NOT_SENT.lock().unwrap().0.contains_key(&key),
            "a send that was mined is still said not to go out"
        );
    }

    /// A keeper whose account can't pay for gas has every call turned away
    /// for the same reason: that is said once, with a count every ten
    /// minutes after, not once for each call.
    #[tokio::test(flavor = "current_thread")]
    async fn a_keeper_out_of_gas_money_is_said_once_however_many_calls_fail() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let node = node(BETWEEN_SLOTS);
        let client = crate::stand_in::connect_turning_away_then_reverting(
            &node,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        for claim in 1..=3u64 {
            let freeze = IVault::freezeCall {
                claimId: U256::from(claim),
            };
            assert_eq!(
                send_outcome(&client, KEEPER, VAULT, freeze, "freeze").await,
                Outcome::NotSent
            );
        }
        assert_eq!(
            send_outcome(
                &client,
                KEEPER,
                STATION,
                IStation::stop_0Call {},
                "stop the station"
            )
            .await,
            Outcome::NotSent
        );
        assert_eq!(
            logged
                .text()
                .matches("the keeper's send did not go out")
                .count(),
            1,
            "{}",
            logged.text()
        );
        let calls = NOT_SENT
            .lock()
            .unwrap()
            .0
            .keys()
            .filter(|(rpc, ..)| *rpc == client.rpc_url)
            .count();
        assert_eq!(calls, 4);
    }

    /// While sends are failing the health route doesn't answer as up: a
    /// dashboard or supervisor that reads only the status hears of it. Its
    /// first line stays short, for a dashboard's row, however many kinds of
    /// send fail; what they are follows.
    #[tokio::test]
    async fn a_keeper_whose_sends_are_not_going_out_does_not_answer_its_health_check_as_up() {
        let now = std::time::Instant::now();
        {
            let mut not_sent = NOT_SENT.lock().unwrap();
            for what in [
                "value",
                "settle",
                "serve a claim",
                "keep",
                "unwind the station",
            ] {
                not_sent.failed(
                    (
                        "health".to_string(),
                        Address::repeat_byte(0xa3),
                        what.to_string(),
                        B256::ZERO,
                        [0; 4],
                    ),
                    now,
                );
            }
        }
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        tokio::spawn(serve_status(SocketAddr::from(([127, 0, 0, 1], port))));
        let mut reply = None;
        for _ in 0..50 {
            if let Ok(answer) = reqwest::get(format!("http://127.0.0.1:{port}/health")).await {
                reply = Some(answer);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let reply = reply.expect("the health route answers");
        assert_eq!(reply.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let text = reply.text().await.unwrap();
        let first = text.lines().next().unwrap_or_default();
        assert!(
            first.starts_with("keeper: ") && first.len() <= 60,
            "{text:?}"
        );
        assert!(text.contains("serve a claim"), "{text:?}");

        // An executor that can't be asked is said the same way.
        let (up, said) = health_with(
            &Failures::default(),
            &[(31337, VAULT)],
            &std::collections::BTreeMap::new(),
            now,
        );
        assert!(!up);
        assert_eq!(said.lines().next(), Some("keeper: executor not answering"));
        assert!(said.contains(&format!("31337/{VAULT:?}")), "{said}");
    }

    /// The keeper's account has run out of gas money: every send it makes is
    /// turned away by the node ("insufficient funds"). That is not the vault
    /// refusing, and a keeper that can't send values, settles, serves and
    /// bills nothing, so it is heard of: said at warn and in the keeper's
    /// health, until the send goes out again.
    #[tokio::test(flavor = "current_thread")]
    async fn a_send_the_node_turns_away_for_want_of_gas_is_heard_of_until_it_goes_out() {
        let node = node(SLOT_START);
        an_open_vault(&node, a_tank_holding(50_000_000));
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let turning_away = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_turning_away::<IVault::recordValuationCall>(
            &node,
            serde_json::json!({"code": -32000, "message": "insufficient funds for gas * price + value: balance 0, tx cost 21000000000000"}),
            turning_away.clone(),
        )
        .await;
        let failing = (
            client.rpc_url.clone(),
            VAULT,
            "value".to_string(),
            alloy::primitives::keccak256(IVault::recordValuationCall {}.abi_encode()),
            IVault::recordValuationCall::SELECTOR,
        );
        let heard = || NOT_SENT.lock().unwrap().0.contains_key(&failing);
        let executor = executor_serving_nothing().await;
        let round = || async {
            let mut memory = Memory {
                daily_at: Some(SLOT_START - 60),
                ..Memory::default()
            };
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        };

        round().await;
        assert_eq!(
            sent::<IVault::recordValuationCall>(&node, VAULT),
            0,
            "the valuation was meant to be turned away: {:?}",
            everything_sent(&node)
        );
        assert!(
            heard(),
            "a valuation turned away for want of gas went unheard of"
        );
        assert!(NOT_SENT.lock().unwrap().whats().contains("value"));

        // The keeper is paid for again: the valuation goes out, and is no
        // longer said to be failing.
        turning_away.store(false, Ordering::SeqCst);
        round().await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
        assert!(!heard());
    }

    /// `keep_one` tries each step on its own: a read one step needs that
    /// fails holds up that step, not the round. Here the tank, the halt, or
    /// the home station can't be read in the slot's window (as a
    /// rate-limited or flaky node's reads fail), and the vault is valued
    /// all the same.
    #[tokio::test]
    async fn a_vault_whose_tank_halt_or_station_cannot_be_read_is_still_valued() {
        for unread in ["tank", "halt", "home station"] {
            let node = node(SLOT_START);
            an_open_vault(&node, a_tank_holding(50_000_000));
            answers(&node, STATION, IStation::outsideBandCall {}, false);
            match unread {
                "tank" => reverts(&node, VAULT, IVault::tankCall {}, "unread"),
                "halt" => reverts(&node, VAULT, IStationVault::haltedAtCall {}, "unread"),
                _ => reverts(&node, VAULT, IVault::homeStationCall {}, "unread"),
            }
            let client = connect(&node).await;
            let mut memory = Memory {
                daily_at: Some(SLOT_START - 60),
                ..Memory::default()
            };

            let _ = keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor_serving_nothing().await,
                VAULT,
                &mut memory,
            )
            .await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                1,
                "a failed read of the {unread} kept the vault from being valued in its window; sent: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// A tank that can't be read is no reason to call the day's bill done:
    /// the daily round waits for a round that can read it.
    #[tokio::test]
    async fn a_daily_round_whose_tank_cannot_be_read_is_left_for_the_next_round() {
        let node = node(BETWEEN_SLOTS);
        an_open_vault(&node, a_tank_holding(50_000_000));
        reverts(&node, VAULT, IVault::tankCall {}, "unread");
        let client = connect(&node).await;
        let mut memory = Memory::default();

        let _ = keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor_serving_nothing().await,
            VAULT,
            &mut memory,
        )
        .await;

        assert_eq!(memory.daily_at, None);
    }

    /// An open vault whose tank can't be read round after round is said at
    /// warn once it has failed three rounds in a row, not every round, and in
    /// the keeper's health; a read that works takes it off.
    #[tokio::test(flavor = "current_thread")]
    async fn a_tank_unreadable_round_after_round_is_said_until_it_is_read() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let now = BETWEEN_SLOTS;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 0, now - 3_600));
        reverts(&node, VAULT, IVault::tankCall {}, "unreadable");
        let client = connect(&node).await;
        let executor = executor_serving_nothing().await;
        let mut memory = Memory::default();
        let said = || logged.text().matches("round after round").count();

        for _ in 0..2 {
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }
        assert_eq!(said(), 0, "a tank unread now and then is a node's hiccup");
        for _ in 0..3 {
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }
        assert_eq!(memory.tank_unread, 5);
        assert_eq!(said(), 1, "{}", logged.text());

        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 0, now - 3_600),
        );
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        assert_eq!(memory.tank_unread, 0);
        assert!(logged.text().contains("no longer said to be unreadable"));
    }

    /// A closed vault whose stopped station holds a remnant the venue reports
    /// at the mark but never sells (exit value zero, so the unwind is not
    /// reported blocked): its simulation brings nothing home and leaves the
    /// station as it was. No unwind is paid for in three hours of rounds, one
    /// a slot, and the vault may go quiet.
    #[tokio::test]
    async fn a_remnant_no_unwind_ever_moves_is_never_paid_for() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        a_closed_vault(&node, start - DAY, true, a_tank_holding(0));
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        answers(
            &node,
            STATION,
            IStation::specCall {},
            a_spec(vec![name("local")]),
        );
        answers(
            &node,
            STATION,
            IStation::approvedKeyCall {
                driver: name("local"),
            },
            Address::ZERO,
        );
        // Five cents at the mark, nothing at the bid; never sold.
        a_stopped_station_at(&node, 50_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 50_000);
        let client = connect(&node).await;
        // First seen closed in the first round, its window's wall clock
        // long run: the window is judged here on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((start, a_hold_ago())),
            ..Memory::default()
        };
        let interval = timers().slotInterval;
        for slot in 0..3 * 3_600 / interval {
            node.lock().unwrap().timestamp = start + slot * interval;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            0,
            "{:?}",
            everything_sent(&node)
        );
        assert!(memory.quiet_since.is_some());
    }

    /// A halted vault's stopped station holds a remnant no unwind sells, for
    /// three hours; then its market takes it, and its simulation brings
    /// money home. It is unwound within the simulations' longest wait, five
    /// minutes, of becoming sellable, and never before.
    #[tokio::test]
    async fn a_remnant_that_becomes_sellable_is_unwound_soon_after() {
        let node = node(BETWEEN_SLOTS);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        a_stopped_station_at(&node, 50_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 50_000);
        let mut memory = Memory::default();
        let interval = timers().slotInterval;
        let sellable_from = 3 * 3_600 / interval;
        let mut sent_in = Vec::new();
        for slot in 0..sellable_from + MOST_SECONDS_BETWEEN_CHECKS / interval + 1 {
            if slot == sellable_from {
                a_stopped_station_at(&node, 50_000, 40_000, 40_000, false);
            }
            node.lock().unwrap().timestamp = BETWEEN_SLOTS + slot * interval;
            let before = sent::<IStation::unwindCall>(&node, STATION);
            unwind_round(&node, &mut memory).await;
            if sent::<IStation::unwindCall>(&node, STATION) > before {
                sent_in.push(slot);
            }
        }
        assert!(
            sent_in.first().is_some_and(|first| *first >= sellable_from),
            "unwound in slots {sent_in:?}, sellable from slot {sellable_from}"
        );
        // Once it sells, it is asked again every slot.
        let first = sent_in[0];
        assert_eq!(
            sent_in,
            (first..sellable_from + MOST_SECONDS_BETWEEN_CHECKS / interval + 1).collect::<Vec<_>>()
        );
    }

    /// Wait until every ask of the executor has come back.
    async fn asks_back(executor: &str) {
        let executor = executor_for(executor);
        for _ in 0..500 {
            if executor.asks.lock().unwrap().in_flight.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("an ask of the executor did not come back");
    }

    /// Whether the executor's latest ask about the vault went unanswered.
    fn unanswered(executor: &str, vault: Address) -> bool {
        executor_for(executor)
            .asks
            .lock()
            .unwrap()
            .unanswered
            .iter()
            .any(|&(_, asked)| asked == vault)
    }

    /// Ask the executor about the vault and wait for the answer.
    async fn asked(executor: &str, vault: Address) -> Option<Asked> {
        ask(31337, executor, vault, Lane::Daily);
        asks_back(executor).await;
        latest_answer(executor, 31337, vault).map(|(answer, _)| answer)
    }

    /// A 404 from the executor is taken as no total for the vault, but said
    /// once for each vault: a wrong executor URL answers 404 for all of them.
    #[tokio::test(flavor = "current_thread")]
    async fn an_executor_serving_no_total_for_a_vault_is_said_once_for_it() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let executor = executor_serving_nothing().await;
        let warned = || logged.text().matches("serves no total").count();
        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert_eq!(warned(), 1);
        // Another vault's is its own.
        asked(&executor, STATION).await;
        assert_eq!(warned(), 2);
    }

    /// The vault's first ask is its first daily round, made the moment it
    /// opens, before the executor has taken it on: that 404 is said. Once
    /// the executor has served the vault a total, a 404 is said again: the
    /// executor has moved where the keeper's URL no longer reaches, and the
    /// vault's bills would go uncollected.
    #[tokio::test(flavor = "current_thread")]
    async fn a_404_after_the_executor_has_served_the_vault_is_said_again() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let executor = crate::stand_in::executor_serving_a_total_once_between_404s().await;
        let warned = || logged.text().matches("serves no total").count();

        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert_eq!(warned(), 1);
        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::Total(_))
        ));
        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert_eq!(
            warned(),
            2,
            "the executor stopped serving a vault it had served, and nothing was said"
        );
    }

    /// An executor that answers 404 for every vault it is asked about, and
    /// has served none a total, is a URL that is wrong: after an hour of it
    /// the keeper's health says so. Once any vault gets a total it isn't
    /// said, and another vault's 404 doesn't bring it back.
    #[tokio::test(flavor = "current_thread")]
    async fn an_executor_serving_no_vault_is_said_until_any_vault_gets_a_total() {
        let executor = crate::stand_in::executor_serving_a_total_once_between_404s().await;
        let an_hour_on = || std::time::Instant::now() + serving_none_for();
        let serves_none = |at| serving_none(&executor_for(&executor).asks.lock().unwrap(), at);

        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert!(!serves_none(std::time::Instant::now()));
        assert!(
            serves_none(an_hour_on()),
            "an hour of 404s for every vault is not said"
        );
        assert!(matches!(
            asked(&executor, STATION).await,
            Some(Asked::Total(_))
        ));
        assert!(!serves_none(an_hour_on()));
        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert!(
            !serves_none(an_hour_on()),
            "an executor that serves a vault was said to serve none"
        );
    }

    /// An executor that answered 404 for every vault, each of which the
    /// keeper then no longer wants a total for (closed and settled, say): it
    /// isn't said to serve no vault. Only the vaults the keeper still wants a
    /// total for count.
    #[tokio::test(flavor = "current_thread")]
    async fn an_executor_whose_vaults_no_longer_want_a_total_is_not_said_to_serve_none() {
        let executor = executor_serving_nothing().await;
        let an_hour_on = || std::time::Instant::now() + serving_none_for();
        let serves_none = |at| serving_none(&executor_for(&executor).asks.lock().unwrap(), at);

        assert!(matches!(
            asked(&executor, VAULT).await,
            Some(Asked::NotServed)
        ));
        assert!(matches!(
            asked(&executor, STATION).await,
            Some(Asked::NotServed)
        ));
        assert!(serves_none(an_hour_on()), "setup: no vault is served");
        total_not_wanted(&executor, 31337, VAULT);
        assert!(
            serves_none(an_hour_on()),
            "a vault still wanting a total is served none, and that isn't said"
        );
        total_not_wanted(&executor, 31337, STATION);
        assert!(
            !serves_none(an_hour_on()),
            "no vault wants a total, yet the executor is said to serve none"
        );
    }

    /// A step that keeps failing is said at warn at once, then at most every
    /// ten minutes with a count; another chain's, another vault's or another
    /// step's failing is its own. A round of the vault that finishes without
    /// it failing ends it: its next failure is said at once.
    #[test]
    fn a_step_that_keeps_failing_is_said_once_then_every_ten_minutes() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let chain_id = 51_001;
        let vault = Address::repeat_byte(0xc7);
        let said = || logged.text().matches("a step did not go through").count();
        let fail = |chain_id: u64, vault: Address, step: &'static str| {
            carry_on(
                chain_id,
                vault,
                step,
                Err(anyhow::anyhow!("the node is down")),
            );
        };
        for _ in 0..5 {
            fail(chain_id, vault, "read a claim");
            steps_worked(chain_id, vault, true);
        }
        assert_eq!(said(), 1, "a step failing every round is said every round");
        fail(chain_id, vault, "read a request");
        fail(chain_id, Address::repeat_byte(0xc8), "read a claim");
        fail(chain_id + 1, vault, "read a claim");
        assert_eq!(
            said(),
            4,
            "another chain's, vault's or step's failing is not said"
        );
        assert!(
            logged
                .text()
                .contains(&format!("chain_id={}", chain_id + 1)),
            "the warning names no chain: {}",
            logged.text()
        );

        // A round that doesn't finish says nothing of whether it works.
        steps_worked(chain_id, vault, false);
        steps_worked(chain_id, vault, false);
        fail(chain_id, vault, "read a claim");
        assert_eq!(said(), 4);
        // One that finishes without it failing ends it.
        steps_worked(chain_id, vault, true);
        steps_worked(chain_id, vault, true);
        assert!(logged.text().contains("the step goes through again"));
        fail(chain_id, vault, "read a claim");
        assert_eq!(
            said(),
            5,
            "a step failing again after it worked is not said at once"
        );
    }

    /// An executor that can't be asked (a 503 here, as from one restarting
    /// behind a proxy; no connection or no answer in time is the same) is
    /// said at warn. The vault is asked again a chain-minute after the round
    /// that found the ask unanswered, then after two, four and so on up to an
    /// hour, until it is answered; the day's bill is collected the round
    /// after the answer, and nothing more is asked that day.
    #[tokio::test(flavor = "current_thread")]
    async fn a_daily_bill_the_executor_cannot_be_asked_for_is_said_and_asked_again_on_a_backoff() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let now = BETWEEN_SLOTS;
        let node = node(now);
        an_open_vault(
            &node,
            Tank {
                balance: U256::from(50_000_000u64),
                keepAt: U256::ZERO,
                usageSettled: U256::from(1_000u64),
                usageAsOf: now - DAY,
                lastCollection: now - DAY,
                escapeRequestedAt: 0,
                capCarried: U256::ZERO,
                fillPending: false,
            },
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();
        // The first three asks fail.
        let executor = failing_executor(3, 2_000, now).await;
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            memory.ask_again
        };
        let again = |wait: u64, next: u64| Some(AskAgain { wait, next });

        assert_eq!(round(now, &mut memory).await, None);
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);
        let log = logged.text();
        assert!(
            log.lines()
                .any(|line| line.contains("WARN") && line.contains("executor could not be asked")),
            "the day's bill went uncollected because the executor could not be asked, \
             and nothing was said; the log was:\n{log}"
        );
        assert!(unanswered(&executor, VAULT));

        // Found unanswered: asked again a minute on, then two, then four.
        assert_eq!(round(now + 60, &mut memory).await, again(60, now + 120));
        assert_eq!(round(now + 90, &mut memory).await, again(60, now + 120));
        assert_eq!(round(now + 120, &mut memory).await, again(120, now + 240));
        assert_eq!(round(now + 180, &mut memory).await, again(120, now + 240));
        assert_eq!(round(now + 240, &mut memory).await, again(240, now + 480));
        assert!(unanswered(&executor, VAULT));
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);
        // The fourth ask is answered, and the bill collected the round after.
        round(now + 480, &mut memory).await;
        assert!(!unanswered(&executor, VAULT));
        assert_eq!(round(now + 490, &mut memory).await, None);
        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// The wait before an ask that got no answer is made again doubles up to
    /// an hour, and stays there.
    #[test]
    fn an_unanswered_ask_is_made_again_at_most_an_hour_apart() {
        let executor = "http://never-answers.invalid";
        let vault = Address::repeat_byte(0xb4);
        executor_for(executor)
            .asks
            .lock()
            .unwrap()
            .unanswered
            .insert((31337, vault));
        let mut memory = Memory {
            ask_again: Some(AskAgain {
                wait: 40 * MINUTE,
                next: 1_000,
            }),
            ..Memory::default()
        };
        // An ask is spawned, so a runtime is needed; it fails, unheard.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        ask_again(31337, executor, vault, Lane::Daily, 1_000, &mut memory);
        assert_eq!(
            memory.ask_again,
            Some(AskAgain {
                wait: MOST_SECONDS_BETWEEN_ASKS,
                next: 1_000 + MOST_SECONDS_BETWEEN_ASKS,
            })
        );
    }

    /// A node for a client that signs for itself: it answers the fillers'
    /// questions, and refuses the gas estimate (`estimate`) or the raw send
    /// (`raw`) with the error given.
    async fn a_signing_node(
        estimate: Option<serde_json::Value>,
        raw: Option<serde_json::Value>,
    ) -> String {
        use serde_json::{Value, json};
        type Script = (Option<Value>, Option<Value>);
        fn answer((estimate, raw): &Script, asked: &Value) -> Value {
            let id = asked["id"].clone();
            let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
            let err = |error: &Value| json!({"jsonrpc": "2.0", "id": id, "error": error});
            match asked["method"].as_str().unwrap_or_default() {
                "eth_chainId" => ok(json!("0x7a69")),
                "eth_getTransactionCount" => ok(json!("0x0")),
                "eth_gasPrice" | "eth_maxPriorityFeePerGas" => ok(json!("0x3b9aca00")),
                "eth_blockNumber" => ok(json!("0x10")),
                "eth_feeHistory" => ok(json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]]
                })),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                    ok(serde_json::to_value(block).unwrap())
                }
                "eth_estimateGas" => match estimate {
                    Some(error) => err(error),
                    None => ok(json!("0x5208")),
                },
                "eth_sendRawTransaction" => match raw {
                    Some(error) => err(error),
                    None => ok(json!(B256::repeat_byte(0xab))),
                },
                other => err(&json!({"code": -32601, "message": format!("not scripted: {other}")})),
            }
        }
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    |axum::extract::State(script): axum::extract::State<Arc<Script>>,
                     axum::Json(body): axum::Json<Value>| async move {
                        axum::Json(match body {
                            Value::Array(batch) => {
                                Value::Array(batch.iter().map(|one| answer(&script, one)).collect())
                            }
                            one => answer(&script, &one),
                        })
                    },
                ),
            )
            .with_state(Arc::new((estimate, raw)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        url
    }

    async fn signing_client(url: String) -> (Client, Address) {
        let keys = chain::keys_from_mnemonic(
            "test test test test test test test test test test test junk",
            1,
        )
        .unwrap();
        let address = keys[0].address();
        let config = chain::ChainConfig {
            label: "signing stand-in".to_string(),
            rpc: url.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: None,
            confirmations: None,
            public_rpc: url,
            logs_rpc: None,
            log_range: None,
        };
        (
            Client::connect(&config, chain::Signing::Keys(keys))
                .await
                .unwrap(),
            address,
        )
    }

    /// With the keys the keeper runs with on a testnet, a refusal comes from
    /// the gas filler's estimate rather than from `eth_sendTransaction` as in
    /// the stand-in. It is still read as the vault refusing, and a node's
    /// own refusal of the raw send is still read as not sent.
    #[tokio::test]
    async fn a_signing_client_reads_refusals_as_the_stand_in_does() {
        let keep = || IVault::keepCall {
            total: U256::from(1u64),
            asOf: 1,
            signature: Bytes::from(vec![0x11; 65]),
        };
        let reverted =
            serde_json::json!({"code": 3, "message": "execution reverted", "data": "0x"});
        let (client, keeper) = signing_client(a_signing_node(Some(reverted), None).await).await;
        assert_eq!(
            send_outcome(&client, keeper, VAULT, keep(), "keep").await,
            Outcome::Refused
        );

        let broke = serde_json::json!({"code": -32000, "message": "insufficient funds for gas * price + value"});
        let (client, keeper) = signing_client(a_signing_node(None, Some(broke)).await).await;
        assert_eq!(
            send_outcome(&client, keeper, VAULT, keep(), "keep").await,
            Outcome::NotSent
        );
    }

    #[test]
    fn a_claim_left_waiting_is_tried_ever_less_often_until_its_state_changes() {
        let interval = 10;
        let mut tried: Option<Tried> = None;
        let mut slot = 0;
        let mut waits = Vec::new();
        let mut round = 0;
        for _ in 0..9 {
            assert!(claim_due(tried.as_ref(), ClaimState::Waiting, slot, round));
            let next = next_try(tried.as_ref(), ClaimState::Waiting, slot, interval, round);
            // Not due again before its wait is over, in slots or in rounds.
            assert!(!claim_due(
                Some(&next),
                ClaimState::Waiting,
                next.next - 1,
                next.next_round
            ));
            assert!(!claim_due(
                Some(&next),
                ClaimState::Waiting,
                next.next,
                next.next_round - 1
            ));
            waits.push(next.wait);
            slot = next.next;
            round = next.next_round;
            tried = Some(next);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 64, 64]);

        // A change of state makes it due at once, and starts the count again.
        let tried = tried.unwrap();
        assert!(claim_due(
            Some(&tried),
            ClaimState::Pending,
            tried.next - 1,
            0
        ));
        let after_change = next_try(None, ClaimState::Waiting, slot, interval, round);
        assert_eq!(after_change.wait, 1);
    }

    fn a_stopped_station(node: &crate::stand_in::Shared, sent_home: u64) {
        a_stopped_station_at(node, 100_000_000, 99_000_000, sent_home, false);
    }

    fn a_stopped_station_at(
        node: &crate::stand_in::Shared,
        mark: u64,
        exit: u64,
        sent_home: u64,
        blocked: bool,
    ) {
        answers(node, STATION, IStation::stoppedCall {}, true);
        answers(
            node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::from(mark),
                exit: U256::from(exit),
            },
        );
        answers(
            node,
            STATION,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
            IStation::unwindReturn {
                sentHome: U256::from(sent_home),
                blocked,
            },
        );
    }

    /// Have the Closer's simulated close of the vault go through.
    fn closes(node: &crate::stand_in::Shared, closer: Address) {
        answers(
            node,
            closer,
            ICloser::closeHaltedCall { vault: VAULT },
            ICloser::closeHaltedReturn {},
        );
    }

    async fn unwind_round(node: &crate::stand_in::Shared, memory: &mut Memory) {
        let client = connect(node).await;
        stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            Address::ZERO,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            memory,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_stopped_station_is_settled_for_before_it_is_asked_to_unwind() {
        let node = node(BETWEEN_SLOTS);
        a_stopped_station(&node, 99_000_000);
        answers(&node, VAULT, IStationVault::unsettledCall {}, true);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            Address::ZERO,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(sent::<IVault::settleCall>(&node, VAULT), 1);
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 1);
        assert_eq!(memory.unwound_in, Some(slot_of(BETWEEN_SLOTS, 10)));
    }

    #[tokio::test]
    async fn a_stopped_station_whose_full_unwind_would_bring_money_home_is_asked_to_unwind() {
        let node = node(BETWEEN_SLOTS);
        a_stopped_station(&node, 99_000_000);
        // Weeks after the stop the band a stopped station sells in has
        // widened past the market's discount, though the running band still
        // reads the market as outside it.
        answers(&node, STATION, IStation::outsideBandCall {}, true);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        let closed = stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            Address::ZERO,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            &mut memory,
        )
        .await
        .unwrap();

        assert!(!closed);
        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            1,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn a_stopped_station_whose_unwind_is_blocked_and_would_bring_nothing_home_is_not_asked_to()
     {
        let node = node(BETWEEN_SLOTS);
        a_stopped_station_at(&node, 100_000_000, 99_000_000, 0, true);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();

        unwind_round(&node, &mut memory).await;

        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            0,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    /// A stopped station holding a levered perp account worth something at
    /// the mark and nothing at the bid: its full unwind closes the position
    /// inside the band, the proceeds only repay the account's debt, and
    /// nothing comes home. Yet that unwind is what empties the station, and
    /// an empty station is what lets the vault close (the contract test
    /// `UnwindThatBringsNothingHomeStillEmpties` shows it).
    #[tokio::test]
    async fn a_full_unwind_that_empties_the_station_but_brings_nothing_home_is_still_sent() {
        let node = node(BETWEEN_SLOTS);
        a_stopped_station_at(&node, 6_000_000_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 0);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();

        unwind_round(&node, &mut memory).await;

        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            1,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    /// A node that won't run a call without a contract to call can't run the
    /// probe as creation code; it is run as an overridden contract's code
    /// instead, and its answer judges the unwind as before: one that empties
    /// the station is sent, one that leaves the mark where it was is not.
    #[tokio::test]
    async fn what_an_unwind_leaves_is_simulated_through_a_state_override_where_creation_code_is_refused()
     {
        for (left, sends) in [(0, 1), (6_000_000_000, 0)] {
            let node = node(BETWEEN_SLOTS);
            crate::stand_in::refuses_creation_calls(&node);
            a_stopped_station_at(&node, 6_000_000_000, 0, 0, false);
            leaves_after_unwind(&node, STATION, left);
            answers(&node, VAULT, IStationVault::unsettledCall {}, false);
            let mut memory = Memory::default();

            unwind_round(&node, &mut memory).await;

            assert_eq!(
                sent::<IStation::unwindCall>(&node, STATION),
                sends,
                "leaving {left}: {:?}",
                everything_sent(&node)
            );
            assert_eq!(memory.probes_failing, 0, "leaving {left}");
        }
    }

    /// A stopped station holding a levered account worth $60 at the mark and
    /// nothing at the bid: only a simulation of what its unwind leaves can
    /// tell whether the unwind empties it. While the node fails that
    /// simulation in both its forms, nothing is sent, however long; the
    /// simulation is made again every slot, and past [`MOST_PROBES_FAILING`]
    /// of them the vault is said at warn. Once one works, the unwind that
    /// empties the station is sent, and the failures are forgotten.
    #[tokio::test(flavor = "current_thread")]
    async fn an_unwind_whose_simulation_keeps_failing_is_never_sent_and_is_said() {
        let (logged, _guard) = crate::stand_in::Logged::capture();
        let node = node(BETWEEN_SLOTS);
        crate::stand_in::refuses_simulations(&node);
        a_stopped_station_at(&node, 60_000_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 0);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();
        let interval = timers().slotInterval;
        let slots = MOST_PROBES_FAILING + 10;
        for slot in 0..slots {
            node.lock().unwrap().timestamp = BETWEEN_SLOTS + slot * interval;
            unwind_round(&node, &mut memory).await;
        }
        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            0,
            "an unwind went out unsimulated: {:?}",
            everything_sent(&node)
        );
        assert_eq!(
            memory.probes_failing, slots,
            "not simulated again every slot"
        );
        let warned = logged.text().matches("can't be simulated").count();
        assert_eq!(warned, 1, "{}", logged.text());

        crate::stand_in::takes_simulations(&node);
        node.lock().unwrap().timestamp = BETWEEN_SLOTS + slots * interval;
        unwind_round(&node, &mut memory).await;
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 1);
        assert_eq!(memory.probes_failing, 0);
    }

    /// A vault whose unwind's simulation has failed [`MOST_PROBES_FAILING`]
    /// times in a row is in the keeper's health, and leaves it once one works
    /// or the vault is no longer listed.
    #[tokio::test]
    async fn a_vault_whose_simulations_keep_failing_is_in_the_health_until_one_works() {
        let node = node(BETWEEN_SLOTS);
        crate::stand_in::refuses_simulations(&node);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        let client = connect(&node).await;
        let mut memory = HashMap::from([(
            VAULT,
            Memory {
                probes_failing: MOST_PROBES_FAILING,
                ..Memory::default()
            },
        )]);
        let key = (client.chain_id, VAULT);
        let factory = deployments().factory;
        answers(&node, factory, IVaultFactory::vaultsCall {}, vec![VAULT]);
        let _ = tick(
            Chain::Base,
            &client,
            &deployments(),
            KEEPER,
            "http://127.0.0.1:1",
            &mut memory,
            &mut Factories::default(),
        )
        .await;
        assert_eq!(
            UNPROBED.lock().unwrap().get(&key),
            Some(&MOST_PROBES_FAILING)
        );

        memory.get_mut(&VAULT).unwrap().probes_failing = 0;
        let _ = tick(
            Chain::Base,
            &client,
            &deployments(),
            KEEPER,
            "http://127.0.0.1:1",
            &mut memory,
            &mut Factories::default(),
        )
        .await;
        assert!(!UNPROBED.lock().unwrap().contains_key(&key));
    }

    /// A chain's node that can't run the simulation in either form is found
    /// out when the chain connects; one that runs either form is not.
    #[tokio::test]
    async fn a_node_that_cannot_simulate_is_found_out() {
        let node = node(BETWEEN_SLOTS);
        let client = connect(&node).await;
        assert!(can_simulate(&client).await.is_ok());
        crate::stand_in::refuses_creation_calls(&node);
        assert!(can_simulate(&client).await.is_ok());
        crate::stand_in::refuses_simulations(&node);
        assert!(can_simulate(&client).await.is_err());
    }

    /// A stopped station's unwind sells each driver's holdings inside
    /// try/catch, and a sale that runs out of gas is caught rather than
    /// failing the call, so with only the node's estimate a sale can be
    /// swallowed. The unwind and the Closer are sent with half again the
    /// estimate and a margin besides, under the block's gas limit.
    #[tokio::test]
    async fn an_unwind_and_the_closer_are_sent_with_gas_well_above_the_estimate() {
        // The stand-in estimates every call at 21,000.
        let roomy = Some(21_000 + 10_500 + ROOMY_GAS_MARGIN);
        let unwinding = node(BETWEEN_SLOTS);
        a_stopped_station(&unwinding, 99_000_000);
        answers(&unwinding, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();
        unwind_round(&unwinding, &mut memory).await;
        assert_eq!(
            crate::stand_in::gas_limits_of::<IStation::unwindCall>(&unwinding, STATION),
            vec![roomy]
        );

        let closer = Address::repeat_byte(0x66);
        let node = node(SLOT_START);
        closes(&node, closer);
        halted_empty_unvalued(&node, SLOT_START);
        let client = connect(&node).await;
        keep_one(
            &client,
            KEEPER,
            &Deployments {
                closer,
                ..deployments()
            },
            "http://127.0.0.1:1",
            VAULT,
            &mut Memory::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::stand_in::gas_limits_of::<ICloser::closeHaltedCall>(&node, closer),
            vec![roomy]
        );

        // Never past the block's limit, and never under the estimate.
        assert_eq!(roomy_gas(1_000_000, None), 1_750_000);
        assert_eq!(roomy_gas(1_000_000, Some(1_200_000)), 1_200_000);
        assert_eq!(roomy_gas(1_000_000, Some(900_000)), 1_000_000);
        assert_eq!(roomy_gas(u64::MAX, None), u64::MAX);
    }

    /// A venue that reports value it never closes, its mark wobbling a
    /// dollar-unit each slot as a perp's does: every simulation brings
    /// nothing home and leaves the mark where it was. No unwind is paid for.
    #[tokio::test]
    async fn an_unwind_that_would_do_nothing_is_not_paid_for_while_the_mark_moves() {
        let node = node(BETWEEN_SLOTS);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();
        let interval = timers().slotInterval;

        for slot in 0..16u64 {
            node.lock().unwrap().timestamp = BETWEEN_SLOTS + slot * interval;
            let mark = 6_000_000_000 + slot % 2;
            a_stopped_station_at(&node, mark, 0, 0, false);
            leaves_after_unwind(&node, STATION, mark);
            unwind_round(&node, &mut memory).await;
        }
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 0);
    }

    /// The same remnant, its mark a live perp's: $6,000, wandering a cent
    /// either way from one reading to the next (price, funding), as a
    /// position that size does in ten seconds, and a cent lower after the
    /// unwind's own block than before it. The mark's readings never send an
    /// unwind; what the simulation leaves does.
    #[tokio::test]
    async fn a_mark_that_ticks_down_across_the_unwind_does_not_have_it_sent() {
        let node = node(BETWEEN_SLOTS);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let mut memory = Memory::default();
        let interval = timers().slotInterval;

        for slot in 0..16u64 {
            node.lock().unwrap().timestamp = BETWEEN_SLOTS + slot * interval;
            let mark = 6_000_000_000 + (slot % 2) * UNWIND_WORTH_AT_LEAST;
            a_stopped_station_at(&node, mark, 0, 0, false);
            leaves_after_unwind(&node, STATION, mark - UNWIND_WORTH_AT_LEAST);
            unwind_round(&node, &mut memory).await;
        }
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 0);
    }

    #[tokio::test]
    async fn an_emptying_unwind_that_never_went_out_is_tried_again_next_slot() {
        let interval = timers().slotInterval;
        let node = node(BETWEEN_SLOTS);
        a_stopped_station_at(&node, 6_000_000_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 0);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let unwind = IStation::unwindCall {
            fraction: U256::from(WAD),
        };
        refuses(&node, STATION, unwind.clone());
        let mut memory = Memory::default();

        unwind_round(&node, &mut memory).await;
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 0);

        // Next slot, the same mark, and the node takes it now.
        takes(&node, STATION, unwind);
        node.lock().unwrap().timestamp = BETWEEN_SLOTS + interval;
        unwind_round(&node, &mut memory).await;

        assert_eq!(
            sent::<IStation::unwindCall>(&node, STATION),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    fn a_station_that_cannot_be_valued(node: &crate::stand_in::Shared, sent_home: u64) {
        answers(node, STATION, IStation::stoppedCall {}, true);
        reverts(
            node,
            STATION,
            IStation::valueCall {},
            "the venue's read reverts",
        );
        answers(
            node,
            STATION,
            IStation::specCall {},
            StationSpec {
                vaultChain: U256::from(31337u64),
                vault: VAULT,
                bundleHash: B256::ZERO,
                drivers: vec![name("local")],
                bridges: vec![],
                chains: vec![],
                exitBandBps: 100,
            },
        );
        answers(
            node,
            STATION,
            IStation::unwindStoppedCall {
                drivers: vec![name("local")],
            },
            IStation::unwindStoppedReturn {
                sentHome: U256::from(sent_home),
                blocked: sent_home == 0,
            },
        );
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
    }

    #[tokio::test]
    async fn an_unwind_through_named_drivers_that_would_bring_nothing_home_is_not_sent() {
        let node = node(BETWEEN_SLOTS);
        // The one driver can be asked, and would bring nothing home: its
        // market is outside the band and the station holds no idle dollars.
        a_station_that_cannot_be_valued(&node, 0);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            Address::ZERO,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<IStation::unwindStoppedCall>(&node, STATION),
            0,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn an_unwind_through_named_drivers_that_would_bring_money_home_is_sent() {
        let node = node(BETWEEN_SLOTS);
        a_station_that_cannot_be_valued(&node, 5_000_000);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            Address::ZERO,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(sent::<IStation::unwindStoppedCall>(&node, STATION), 1);
    }

    fn a_waiting_claim(node: &crate::stand_in::Shared, now: u64, state: ClaimState) {
        answers(node, VAULT, IVault::claimCountCall {}, U256::from(1u64));
        answers(
            node,
            VAULT,
            IVault::claimAtCall {
                claimId: U256::from(1u64),
            },
            Claim {
                owner: Address::repeat_byte(0x44),
                shares: U256::from(WAD),
                bookedAt: now - 100,
                lastAttemptAt: 0,
                state,
                paid: U256::ZERO,
            },
        );
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
    }

    /// A claim's try values the home station and stages its close inside
    /// try/catch, and counts a call that ran out of gas as blocked; the
    /// node's estimate finds exactly that cheaper path. So a claim is tried
    /// with half again the estimate and a margin, as an unwind is.
    #[tokio::test]
    async fn a_claim_is_tried_with_gas_well_above_the_estimate() {
        // The stand-in estimates every call at 21,000.
        let roomy = Some(21_000 + 10_500 + ROOMY_GAS_MARGIN);
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Pending);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            crate::stand_in::gas_limits_of::<IVault::settleClaimCall>(&node, VAULT),
            vec![roomy]
        );
    }

    /// A holder's units are served by a close staged inside try/catch, like a
    /// claim's: sent with the same room above the estimate.
    #[tokio::test]
    async fn a_holders_units_are_served_with_gas_well_above_the_estimate() {
        let roomy = Some(21_000 + 10_500 + ROOMY_GAS_MARGIN);
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let holder = Address::repeat_byte(0x02);
        answers(
            &node,
            VAULT,
            IVault::unitsOfCall { holder },
            IVault::unitsOfReturn {
                units: U256::from(5u64),
                claimable: U256::ZERO,
            },
        );
        answers(&node, VAULT, IVault::unitsSinceCall { holder }, now);
        let client = connect(&node).await;
        let mut memory = Memory {
            holders: BTreeSet::from([holder]),
            ..Memory::default()
        };

        units(&client, KEEPER, VAULT, 1, now, 7 * DAY, 10, 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            crate::stand_in::gas_limits_of::<IVault::serveProRataCall>(&node, VAULT),
            vec![roomy]
        );
    }

    #[tokio::test]
    async fn a_claim_is_not_tried_while_the_station_is_outside_the_band() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Pending);
        answers(&node, STATION, IStation::outsideBandCall {}, true);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            sent::<IVault::settleClaimCall>(&node, VAULT),
            0,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    #[test]
    fn the_claim_a_close_was_last_staged_for_takes_its_turn_after_the_others() {
        assert_eq!(turn_order(1, 3, None), vec![1, 2, 3]);
        assert_eq!(turn_order(1, 3, Some(1)), vec![2, 3, 1]);
        assert_eq!(turn_order(1, 3, Some(2)), vec![1, 3, 2]);
        // One already past the settled prefix changes nothing.
        assert_eq!(turn_order(2, 3, Some(1)), vec![2, 3]);
        assert_eq!(turn_order(1, 0, Some(1)), Vec::<u64>::new());
    }

    #[test]
    fn only_a_claim_booked_or_waiting_is_ever_tried() {
        assert!(claim_tryable(ClaimState::Pending));
        assert!(claim_tryable(ClaimState::Waiting));
        // Priced by a valuation, paid by its own close's delivery, or done.
        assert!(!claim_tryable(ClaimState::Queued));
        assert!(!claim_tryable(ClaimState::Closing));
        assert!(!claim_tryable(ClaimState::Paid));
        assert!(!claim_tryable(ClaimState::Fallback));
    }

    #[tokio::test]
    async fn a_claim_that_is_closing_is_never_tried() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Closing);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(sent::<IVault::settleClaimCall>(&node, VAULT), 0);
    }

    #[tokio::test]
    async fn no_claim_is_tried_while_a_close_is_staged_at_the_station() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Waiting);
        answers(
            &node,
            STATION,
            IStation::stageCall {},
            chain::bindings::station::Stage {
                fraction: U256::from(WAD / 2),
                owed: U256::ZERO,
                estimate: U256::from(1_000u64),
                fetched: U256::ZERO,
                cost: U256::ZERO,
                part: U256::ZERO,
                arrivedAt: U256::ZERO,
                stagedAt: now - 5,
                stagedBlock: 1,
                sweptAt: 0,
                sweptBlock: 0,
                window: 300,
                delay: 0,
                forVault: true,
            },
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            sent::<IVault::settleClaimCall>(&node, VAULT),
            0,
            "the vault would refuse it: nothing is sent"
        );
    }

    #[tokio::test]
    async fn a_claim_is_tried_when_the_band_cannot_be_read() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Pending);
        reverts(
            &node,
            STATION,
            IStation::outsideBandCall {},
            "a driver's read reverts",
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(sent::<IVault::settleClaimCall>(&node, VAULT), 1);
    }

    #[tokio::test]
    async fn a_claim_that_keeps_waiting_is_tried_ever_less_often() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        // Every try leaves it waiting.
        a_waiting_claim(&node, start, ClaimState::Waiting);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let interval = timers().slotInterval;

        let mut tried_in = Vec::new();
        for slot in 0..16u64 {
            let before = sent::<IVault::settleClaimCall>(&node, VAULT);
            node.lock().unwrap().timestamp = start + slot * interval;
            claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
                .await
                .unwrap();
            if sent::<IVault::settleClaimCall>(&node, VAULT) > before {
                tried_in.push(slot);
            }
        }
        assert_eq!(tried_in, [0, 1, 3, 7, 15]);
    }

    #[tokio::test]
    async fn a_claim_whose_try_the_node_refuses_is_tried_again_next_slot() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        a_waiting_claim(&node, start, ClaimState::Waiting);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        // Refused when its gas is estimated: it never reaches the chain.
        refuses(
            &node,
            VAULT,
            IVault::settleClaimCall {
                claimId: U256::from(1u64),
            },
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let interval = timers().slotInterval;

        let mut due_in = Vec::new();
        for slot in 0..6u64 {
            node.lock().unwrap().timestamp = start + slot * interval;
            claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
                .await
                .unwrap();
            if memory.claims_tried.get(&1).is_some_and(|tried| {
                tried.next == slot_of(start + slot * interval, interval) + interval
            }) {
                due_in.push(slot);
            }
        }
        assert_eq!(due_in, [0, 1, 2, 3, 4, 5], "the wait never grew");
        assert_eq!(memory.claims_tried.get(&1).map(|tried| tried.wait), Some(1));
    }

    #[tokio::test]
    async fn a_claim_is_judged_on_the_chains_clock_as_it_is_when_claims_are_tried() {
        // The round began in one slot and has reached the next by the time
        // the claims are tried: the try counts for the slot it is made in.
        let node = node(BETWEEN_SLOTS + timers().slotInterval);
        a_waiting_claim(&node, BETWEEN_SLOTS, ClaimState::Waiting);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        let slot = slot_of(BETWEEN_SLOTS + timers().slotInterval, timers().slotInterval);
        assert_eq!(
            memory.claims_tried.get(&1).map(|tried| tried.next),
            Some(slot + timers().slotInterval)
        );
    }

    fn a_spec(drivers: Vec<B256>) -> StationSpec {
        StationSpec {
            vaultChain: U256::from(31337u64),
            vault: VAULT,
            bundleHash: B256::ZERO,
            drivers,
            bridges: vec![],
            chains: vec![],
            exitBandBps: 100,
        }
    }

    /// An open vault with nothing waiting, due its daily round.
    fn an_open_vault(node: &crate::stand_in::Shared, tank: Tank) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Open);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::tankCall {}, tank);
        answers(node, VAULT, IStationVault::haltedAtCall {}, 0);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    }

    async fn round(node: &crate::stand_in::Shared, deployments: &Deployments, executor: &str) {
        let client = connect(node).await;
        let mut memory = Memory::default();
        keep_one(&client, KEEPER, deployments, executor, VAULT, &mut memory)
            .await
            .unwrap();
    }

    /// A round, and once the executor has answered its asks, another at the
    /// same moment: the answers are collected in a round after the asks.
    async fn billed_round(node: &crate::stand_in::Shared, executor: &str) {
        let client = connect(node).await;
        let mut memory = Memory::default();
        for _ in 0..2 {
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(executor).await;
        }
    }

    #[tokio::test]
    async fn a_signed_total_the_vault_has_already_collected_is_not_sent_in_the_daily_round() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 1_500_000, now - 2 * DAY));
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;

        billed_round(&node, &executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            0,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn a_fill_is_asked_for_only_when_the_author_holds_shares_and_the_vault_holds_cash() {
        let now = BETWEEN_SLOTS;
        let author = Address::repeat_byte(0x77);
        let wants_a_fill = Tank {
            keepAt: U256::from(80_000_000u64),
            ..a_tank(now, 0, now - 2 * DAY)
        };
        for (shares, cash, keeps) in [
            (0u64, 1_000_000u64, 0),
            (1_000_000, 0, 0),
            (1_000_000, 1_000_000, 1),
        ] {
            let node = node(now);
            an_open_vault(&node, wants_a_fill.clone());
            answers(&node, VAULT, IVault::authorCall {}, author);
            answers(
                &node,
                VAULT,
                IVault::balanceOfCall { owner: author },
                U256::from(shares),
            );
            answers(&node, VAULT, IVault::cashCall {}, U256::from(cash));

            // No executor answers at this address.
            round(&node, &deployments(), "http://127.0.0.1:1").await;

            assert_eq!(
                sent::<IVault::keepCall>(&node, VAULT),
                keeps,
                "the author holds {shares}, the vault {cash}; sent: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// The author asks to withdraw the tank. The executor is asked for its
    /// latest at most once a minute, and each total it answers with is
    /// collected; one the vault refuses before it goes out is tried again,
    /// and once one goes through nothing more is asked.
    #[tokio::test]
    async fn an_escape_has_the_executor_asked_at_most_once_a_minute_until_a_keep_goes_through() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let tank = Tank {
            escapeRequestedAt: now - 10,
            ..a_tank(now, 1_000_000, now - 3_600)
        };
        an_open_vault(&node, tank);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        // The vault refuses the collection, so the escape stays uncollected.
        let keep = IVault::keepCall {
            total: U256::ZERO,
            asOf: 0,
            signature: Bytes::new(),
        };
        refuses(&node, VAULT, keep.clone());
        let as_of = Arc::new(AtomicU64::new(now - 60));
        let executor = executor(1_500_000, as_of.clone()).await;
        let client = connect(&node).await;
        let mut memory = Memory {
            daily_at: Some(now - 60),
            ..Memory::default()
        };
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            memory.bill_asked_at
        };

        assert_eq!(round(now, &mut memory).await, Some(now));
        // Within the minute: the answer is tried, and nothing more asked.
        as_of.store(now - 30, Ordering::SeqCst);
        assert_eq!(round(now + 30, &mut memory).await, Some(now));
        // A minute on: asked again.
        assert_eq!(round(now + 60, &mut memory).await, Some(now + 60));
        assert_eq!(memory.escape_collected, 0, "nothing went through");
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);
        // The newer total goes through, and nothing more is asked.
        takes(&node, VAULT, keep);
        assert_eq!(round(now + 90, &mut memory).await, Some(now + 60));
        assert_eq!(memory.escape_collected, now - 10);
        assert_eq!(memory.bill_sent_as_of, now - 30);
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 1);
        assert_eq!(round(now + 200, &mut memory).await, Some(now + 60));
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 1);
    }

    /// The author asks to withdraw the tank after the executor's latest
    /// answer came back. A `keep` with that answer goes through, but leaves
    /// out what the agent used since, so it doesn't settle the escape: the
    /// executor is asked again, and the escape is settled by the `keep` with
    /// the answer to that ask.
    #[tokio::test]
    async fn an_escape_is_not_taken_as_collected_by_an_answer_asked_for_before_it() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let as_of = Arc::new(AtomicU64::new(now - 60));
        let executor = executor(1_500_000, as_of.clone()).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            sent::<IVault::keepCall>(&node, VAULT)
        };

        // The daily round asks.
        assert_eq!(round(now, &mut memory).await, 0);
        // The escape is asked for; the answer in hand is older.
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            Tank {
                escapeRequestedAt: now + 5,
                ..a_tank(now, 1_000_000, now - 3_600)
            },
        );
        as_of.store(now + 8, Ordering::SeqCst);
        assert_eq!(round(now + 10, &mut memory).await, 1);
        assert_eq!(
            memory.escape_collected, 0,
            "an answer asked for before the escape settled it"
        );
        assert_eq!(memory.bill_asked_at, Some(now + 10));
        // The answer to the ask made since settles it.
        assert_eq!(round(now + 20, &mut memory).await, 2);
        assert_eq!(memory.escape_collected, now + 5);
    }

    /// A halted vault's ask, which a `close` waits for, is answered at once
    /// however many open vaults' daily asks are waiting on a slow executor.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_halted_vaults_ask_does_not_queue_behind_open_vaults_daily_asks() {
        let halted = Address::repeat_byte(0xc1);
        let app = axum::Router::new().route(
            "/usage/{chain}/{vault}",
            axum::routing::get(
                move |axum::extract::Path((_, vault)): axum::extract::Path<(u64, Address)>| async move {
                    if vault != halted {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    axum::Json(serde_json::json!({
                        "total": "1000",
                        "as_of": 1_000,
                        "signature": "0x01",
                    }))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let executor = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for n in 0..(2 * ASKS_AT_ONCE as u8) {
            ask(
                31337,
                &executor,
                Address::repeat_byte(0xd0 + n),
                Lane::Daily,
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        ask(31337, &executor, halted, Lane::Halted);
        answer_in_flight(
            &executor,
            31337,
            halted,
            Lane::Halted,
            Duration::from_secs(2),
        )
        .await;

        assert!(
            latest_answer(&executor, 31337, halted).is_some(),
            "the halted vault's ask waited behind the open vaults' daily asks"
        );
    }

    /// A closed vault gone quiet is looked at again once a day, and the
    /// executor asked. The ask failing says nothing new about a bill already
    /// settled, so the vault goes quiet again, and the failed ask, which
    /// comes back after, doesn't count against the executor for the day.
    #[tokio::test]
    async fn a_quiet_closed_vaults_daily_ask_that_fails_does_not_count_against_the_executor() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let node = node(now);
        a_finished_closed_vault(
            &node,
            halted_at,
            a_tank_carrying_cap_past_the_grace(halted_at + 600),
        );
        // Serves no total for the vault, until it fails.
        let failing = Arc::new(AtomicBool::new(false));
        let app = axum::Router::new().route(
            "/usage/{chain}/{vault}",
            axum::routing::get({
                let failing = failing.clone();
                move || {
                    let failing = failing.load(Ordering::SeqCst);
                    async move {
                        if failing {
                            axum::http::StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            axum::http::StatusCode::NOT_FOUND
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let executor = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = connect(&node).await;
        // First seen closed in the first round, its window's wall clock
        // long run: the window is judged here on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((now, a_hold_ago())),
            ..Memory::default()
        };
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
        };
        for minutes in 0..20u64 {
            round(now + minutes * 61, &mut memory).await;
        }
        let quiet = memory.quiet_since.expect("the vault went quiet");

        failing.store(true, Ordering::SeqCst);
        round(quiet + DAY, &mut memory).await;

        assert_eq!(memory.quiet_since, Some(quiet + DAY));
        assert!(
            !unanswered(&executor, VAULT),
            "a quiet vault's failed ask is held against the executor for the day"
        );
    }

    /// The node turns the bill's `keep` away in the round the Closer would
    /// close the halted vault, whose author may take the tank the moment it
    /// closes. The Closer waits for a later round, and goes once the bill has
    /// gone through.
    #[tokio::test]
    async fn the_closer_is_not_sent_before_the_bill_it_was_held_for_goes_through() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let now = SLOT_START;
        let node = node(now);
        halted_and_empty(&node, now, now - 2 * 600);
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        closes(&node, closer);
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;
        let turning_away = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_turning_away::<IVault::keepCall>(
            &node,
            serde_json::json!({"code": -32005, "message": "request rate limit exceeded"}),
            turning_away.clone(),
        )
        .await;
        let mut memory = Memory::default();

        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;
        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer)
                + sent::<IVault::closeCall>(&node, VAULT),
            0,
            "the vault was closed with its bill turned away: {:?}",
            everything_sent(&node)
        );

        turning_away.store(false, Ordering::SeqCst);
        node.lock().unwrap().timestamp = SLOT_START + 10;
        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;

        let sent_in_order = everything_sent(&node);
        let keep = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::keepCall::SELECTOR)
        );
        let close = format!(
            "{closer} {}",
            alloy::hex::encode_prefixed(ICloser::closeHaltedCall::SELECTOR)
        );
        let first_keep = sent_in_order.iter().position(|sent| *sent == keep);
        let first_close = sent_in_order.iter().position(|sent| *sent == close);
        assert!(
            first_keep.is_some() && first_close.is_some() && first_keep < first_close,
            "sent: {sent_in_order:?}"
        );
    }

    /// A halted vault's close held for its bill, its hold over, and the
    /// Closer able to close it. The executor has signed a newer total since
    /// its last answer: the Closer's path asks for it, as `close`'s does, so
    /// the hold's last try carries it before the Closer goes.
    #[tokio::test]
    async fn the_closer_s_path_asks_for_the_latest_total_while_the_close_is_held() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let now = SLOT_START;
        let node = node(now);
        halted_and_empty(&node, now, now - 2 * 600);
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        closes(&node, closer);
        let total = Arc::new(AtomicU64::new(1_500_000));
        let as_of = Arc::new(AtomicU64::new(now - 60));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        ask(
            node.lock().unwrap().chain_id,
            &executor,
            VAULT,
            Lane::Halted,
        );
        asks_back(&executor).await;
        total.store(1_600_000, Ordering::SeqCst);
        as_of.store(now - 30, Ordering::SeqCst);
        let client = connect(&node).await;
        // Held since a hold ago, a reverted `keep`'s wait still running.
        let mut memory = Memory {
            asked_at_halt: true,
            asked_after_grace: true,
            close_held_since: Some((now - MOST_CLOSE_HOLD, a_hold_ago())),
            keep_again: Some(SendAgain {
                wait: 1,
                next: u64::MAX,
                next_round: u64::MAX,
            }),
            ..Memory::default()
        };

        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();

        let keeps = crate::stand_in::sent_calls::<IVault::keepCall>(&node, VAULT);
        assert!(
            keeps
                .first()
                .is_some_and(|keep| keep.total == U256::from(1_600_000u64)),
            "the hold's last try did not carry the newer total: {:?}",
            keeps.iter().map(|keep| keep.total).collect::<Vec<_>>()
        );
        let sent_in_order = everything_sent(&node);
        let keep = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::keepCall::SELECTOR)
        );
        let close = format!(
            "{closer} {}",
            alloy::hex::encode_prefixed(ICloser::closeHaltedCall::SELECTOR)
        );
        let first_keep = sent_in_order.iter().position(|sent| *sent == keep);
        let first_close = sent_in_order.iter().position(|sent| *sent == close);
        assert!(
            first_keep.is_some() && first_close.is_some() && first_keep < first_close,
            "sent: {sent_in_order:?}"
        );
    }

    #[tokio::test]
    async fn the_bill_is_collected_before_the_vault_is_closed() {
        let now = SLOT_START;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        // Collected an hour ago, so the daily round isn't due.
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        answers(&node, VAULT, IStationVault::haltedAtCall {}, now - 2 * 600);
        answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(
            &node,
            STATION,
            IStation::specCall {},
            a_spec(vec![name("local")]),
        );
        answers(
            &node,
            STATION,
            IStation::approvedKeyCall {
                driver: name("local"),
            },
            Address::ZERO,
        );
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;
        let closer = Address::repeat_byte(0x66);
        closes(&node, closer);
        let deployments = Deployments {
            closer,
            ..deployments()
        };

        round(&node, &deployments, &executor).await;

        let sent_in_order = everything_sent(&node);
        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            1,
            "sent: {sent_in_order:?}"
        );
        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "sent: {sent_in_order:?}"
        );
        let keep = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::keepCall::SELECTOR)
        );
        assert_eq!(sent_in_order.first(), Some(&keep), "the bill goes first");
    }

    #[tokio::test]
    async fn a_closed_vault_still_has_its_last_bill_collected() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 2 * DAY),
        );
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        // Halted a day ago: the last collection fell well before the grace's
        // end.
        answers(&node, VAULT, IStationVault::haltedAtCall {}, now - DAY);
        answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;

        billed_round(&node, &executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "sent: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn a_closed_vault_still_has_its_keys_revoked() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 2 * DAY),
        );
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        // Halted a day ago: the last collection fell well before the grace's
        // end.
        answers(&node, VAULT, IStationVault::haltedAtCall {}, now - DAY);
        answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(
            &node,
            STATION,
            IStation::specCall {},
            a_spec(vec![name("local")]),
        );
        answers(
            &node,
            STATION,
            IStation::approvedKeyCall {
                driver: name("local"),
            },
            Address::repeat_byte(0x99),
        );

        round(&node, &deployments(), "http://127.0.0.1:1").await;

        assert_eq!(sent::<IStation::revokeKeyCall>(&node, STATION), 1);
    }

    /// A halted vault past its grace, with an empty station and nothing
    /// owed, on the step-by-step path (no closer).
    fn a_stopping_vault(node: &crate::stand_in::Shared, now: u64) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(
            node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 0, now - 3_600),
        );
        answers(node, VAULT, IStationVault::haltedAtCall {}, now - 2 * 600);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        a_stopped_station_at(node, 0, 0, 0, false);
        // A valuation after the halt has found the station empty: the vault
        // would close.
        answers(node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
    }

    #[tokio::test]
    async fn a_halted_vault_is_valued_only_when_the_valuation_lands_in_the_window_and_closed_whenever()
     {
        for (now, values) in [(SLOT_START, 1), (BETWEEN_SLOTS, 0)] {
            let node = node(now);
            a_stopping_vault(&node, now);

            round(&node, &deployments(), "http://127.0.0.1:1").await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                values,
                "at {now}: {:?}",
                everything_sent(&node)
            );
            // `close` has no window: it is sent either way, and refused when
            // its gas is estimated if it can't go through yet.
            assert_eq!(
                sent::<IVault::closeCall>(&node, VAULT),
                1,
                "at {now}: {:?}",
                everything_sent(&node)
            );
        }
    }

    #[tokio::test]
    async fn a_daily_keep_with_no_total_and_no_fill_wanted_is_not_sent() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Open);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            Tank {
                balance: U256::from(50_000_000u64),
                keepAt: U256::ZERO,
                usageSettled: U256::ZERO,
                usageAsOf: 0,
                lastCollection: now - 2 * DAY,
                escapeRequestedAt: 0,
                capCarried: U256::ZERO,
                fillPending: false,
            },
        );
        answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(
            &node,
            STATION,
            IStation::specCall {},
            StationSpec {
                vaultChain: U256::from(31337u64),
                vault: VAULT,
                bundleHash: B256::ZERO,
                drivers: vec![],
                bridges: vec![],
                chains: vec![],
                exitBandBps: 100,
            },
        );
        answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        // No executor answers at this address.
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            0,
            "sent: {:?}",
            everything_sent(&node)
        );
        // The vault is open, so its list is still read again.
        assert_eq!(sent::<IVault::refreshRestrictedCall>(&node, VAULT), 1);
    }

    /// A killed vault inside or past its grace, its station stopped and
    /// empty, with nothing owed and no claims: all that is left is to value
    /// it and close.
    fn halted_and_empty(node: &Shared, now: u64, halted_at: u64) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(
            node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 0, now - 3_600),
        );
        answers(node, VAULT, IStationVault::haltedAtCall {}, halted_at);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        a_stopped_station_at(node, 0, 0, 0, false);
        // A valuation after the halt has found the station empty: the vault
        // would close.
        answers(node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
    }

    /// Rounds one block apart on a chain of two-second blocks whose head is
    /// one second into a slot, as Base's odd timestamps put it. A valuation
    /// sent one second in lands at the window's last second; one sent later
    /// would land outside it.
    async fn rounds_on_a_two_second_chain(deployments: &Deployments) -> Shared {
        let start = SLOT_START + 1;
        let node = node(start);
        halted_and_empty(&node, start, start - 2 * 600);
        let client = connect_with_blocks_of(&node, 2).await;
        let mut memory = Memory::default();
        // Thirty rounds, one a block, over six slots.
        for _ in 0..30 {
            keep_one(
                &client,
                KEEPER,
                deployments,
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            node.lock().unwrap().timestamp += 2;
        }
        node
    }

    /// Every valuation sent lands, a block later, inside its slot's window.
    fn valued_inside_the_window(node: &Shared, block_time: u64) {
        let timers = timers();
        let landed: Vec<u64> = sent_at::<IVault::recordValuationCall>(node, VAULT)
            .into_iter()
            .map(|at| at + block_time)
            .collect();
        assert!(landed.len() >= 5, "valued: {landed:?}");
        assert!(
            landed
                .iter()
                .all(|at| at - slot_of(*at, timers.slotInterval) <= timers.slotWindow),
            "landed outside the window: {landed:?}"
        );
    }

    #[tokio::test]
    async fn a_halted_empty_vault_is_valued_and_closed_on_a_chain_of_two_second_blocks() {
        let node = rounds_on_a_two_second_chain(&deployments()).await;

        valued_inside_the_window(&node, 2);
        assert!(
            sent::<IVault::closeCall>(&node, VAULT) >= 1,
            "valued every slot but never closed: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn a_halted_empty_vault_is_closed_on_a_chain_of_two_second_blocks_when_the_closer_cannot_be()
     {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        // The Closer's bundle doesn't go through (over a small block's gas,
        // say): the step-by-step path is what closes the vault.
        let node = {
            let start = SLOT_START + 1;
            let node = node(start);
            halted_and_empty(&node, start, start - 2 * 600);
            refuses(&node, closer, ICloser::closeHaltedCall { vault: VAULT });
            let client = connect_with_blocks_of(&node, 2).await;
            let mut memory = Memory::default();
            for _ in 0..30 {
                keep_one(
                    &client,
                    KEEPER,
                    &with_closer,
                    "http://127.0.0.1:1",
                    VAULT,
                    &mut memory,
                )
                .await
                .unwrap();
                node.lock().unwrap().timestamp += 2;
            }
            node
        };

        valued_inside_the_window(&node, 2);
        assert!(
            sent::<IVault::closeCall>(&node, VAULT) >= 1,
            "never closed: {:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn the_one_transaction_close_is_not_tried_when_it_would_land_after_the_window() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        // Two-second blocks, the head two seconds into a three-second window.
        let now = SLOT_START + 2;
        let node = node(now);
        closes(&node, closer);
        halted_and_empty(&node, now, now - 2 * 600);
        let client = connect_with_blocks_of(&node, 2).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &with_closer,
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A halted vault's bill is collected when the halt is first seen, since
    /// anyone may close the vault inside its grace once a valuation finds its
    /// stations empty and its author may then take the tank (the contract
    /// test `HaltedVaultMayCloseInsideItsGrace` shows it); then once the
    /// grace is over, in one collection. The usage cap is cumulative (the
    /// contract test `UsageCapIsCumulative` shows it), so newer totals inside
    /// the grace are left for that one, and once a collection has reached
    /// the grace's end nothing more is sent. Each answer is collected in the
    /// round after the ask.
    #[tokio::test]
    async fn a_halted_vaults_bill_is_collected_at_the_halt_and_once_after_its_grace() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        // Halted a minute ago, with a ten-minute grace: it ends at now + 540.
        let grace_end = now + 540;
        halted_and_empty(&node, now, now - 60);
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        // Nothing has valued the empty station yet: the vault can't close.
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        let as_of = Arc::new(AtomicU64::new(now - 30));
        let executor = executor(1_500_000, as_of.clone()).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            sent::<IVault::keepCall>(&node, VAULT)
        };

        assert_eq!(round(now, &mut memory).await, 0);
        assert_eq!(
            round(now + 10, &mut memory).await,
            1,
            "{:?}",
            everything_sent(&node)
        );
        // Newer totals inside the grace are left for the collection after it.
        as_of.store(now + 100, Ordering::SeqCst);
        assert_eq!(round(now + 120, &mut memory).await, 1);
        as_of.store(now + 400, Ordering::SeqCst);
        assert_eq!(round(now + 420, &mut memory).await, 1);
        // The grace is over: its bill is collected in one go.
        as_of.store(grace_end + 5, Ordering::SeqCst);
        assert_eq!(round(grace_end + 10, &mut memory).await, 1);
        assert_eq!(
            round(grace_end + 20, &mut memory).await,
            2,
            "{:?}",
            everything_sent(&node)
        );
        // That collection reached the grace's end: nothing more is charged.
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, grace_end),
        );
        as_of.store(grace_end + 100, Ordering::SeqCst);
        assert_eq!(round(grace_end + 200, &mut memory).await, 2);
    }

    /// Inside the grace the executor is asked again only when the vault is
    /// about to be closed, and its answer collected before the close.
    #[tokio::test]
    async fn a_halted_vault_closed_inside_its_grace_has_its_bill_collected_first() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        halted_and_empty(&node, now, now - 60);
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        let as_of = Arc::new(AtomicU64::new(now - 30));
        let executor = executor(1_500_000, as_of.clone()).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;
        asks_back(&executor).await;
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);

        // Ten seconds on, a valuation has found the station empty and a
        // newer total is signed: the halt's answer is collected, then the
        // newer one, before the close.
        as_of.store(now + 5, Ordering::SeqCst);
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        node.lock().unwrap().timestamp = now + 10;
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        let keep = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::keepCall::SELECTOR)
        );
        let close = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::closeCall::SELECTOR)
        );
        assert_eq!(
            everything_sent(&node),
            [keep.clone(), keep, close],
            "the bill goes first"
        );
        assert_eq!(memory.bill_sent_as_of, now + 5);
    }

    /// Asked a few seconds before the grace ended and told nothing new, the
    /// keeper asks again once the grace is over, and the one-transaction
    /// close in that round waits for the answer and collects the total signed
    /// as the grace ended first.
    #[tokio::test]
    async fn the_final_total_is_collected_before_the_closer_closes_the_vault() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        // The grace ends at SLOT_START + 15.
        let halted_at = SLOT_START + 15 - 600;
        let first = BETWEEN_SLOTS;
        let collected = first - 120;
        let tank = Tank {
            usageAsOf: collected,
            ..a_tank(first, 1_000_000, first - 3_600)
        };
        let as_of = Arc::new(AtomicU64::new(collected));
        let executor = executor(1_500_000, as_of.clone()).await;
        let node = node(first);
        closes(&node, closer);
        halted_and_empty(&node, first, halted_at);
        answers(&node, VAULT, IVault::tankCall {}, tank);
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        // The Closer closes the vault.
        let hook: Hook = Arc::new(move |node, to, _| {
            if to == closer {
                answers(node, VAULT, IVault::stateCall {}, VaultState::Closed);
            }
        });
        let client = connect_with_hook(&node, hook).await;
        let mut memory = Memory::default();

        // Inside the grace: the executor's total is the one already collected.
        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);

        // The grace ends; the executor signs its last total.
        as_of.store(SLOT_START + 16, Ordering::SeqCst);
        node.lock().unwrap().timestamp = SLOT_START + 20;
        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;

        let keep = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::keepCall::SELECTOR)
        );
        let close = format!(
            "{closer} {}",
            alloy::hex::encode_prefixed(ICloser::closeHaltedCall::SELECTOR)
        );
        assert_eq!(everything_sent(&node), [keep, close]);
    }

    /// A killed vault past its grace, its station stopped and empty, nothing
    /// owed, no claims: all it needs is a valuation that finds it empty, then
    /// `close`. Nothing has valued it since the halt, so `close` can't go
    /// through yet.
    fn halted_empty_unvalued(node: &Shared, now: u64) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(
            node,
            VAULT,
            IVault::tankCall {},
            Tank {
                balance: U256::from(50_000_000u64),
                keepAt: U256::ZERO,
                usageSettled: U256::ZERO,
                usageAsOf: now - 2 * DAY,
                lastCollection: now,
                escapeRequestedAt: 0,
                capCarried: U256::ZERO,
                fillPending: false,
            },
        );
        answers(node, VAULT, IStationVault::haltedAtCall {}, now - 2 * 600);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(
            node,
            STATION,
            IStation::specCall {},
            StationSpec {
                vaultChain: U256::from(31337u64),
                vault: VAULT,
                bundleHash: B256::ZERO,
                drivers: vec![],
                bridges: vec![],
                chains: vec![],
                exitBandBps: 100,
            },
        );
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        answers(node, STATION, IStation::outsideBandCall {}, false);
        answers(node, STATION, IStation::stoppedCall {}, true);
        answers(
            node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
        answers(
            node,
            STATION,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
            IStation::unwindReturn {
                sentHome: U256::ZERO,
                blocked: false,
            },
        );
        reverts(node, VAULT, IVault::closeCall {}, "not yet");
    }

    /// A call to the deployment's Closer that goes through closed the vault
    /// only if the vault then reads closed. One to an address with nothing
    /// deployed (a deployment file naming a Closer this chain lacks) goes through
    /// and closes nothing; taken as a close, the round would skip the valuation
    /// every slot, and the vault would never close.
    #[tokio::test]
    async fn a_closer_call_that_closed_nothing_does_not_stop_the_vault_being_valued() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let node = node(SLOT_START);
        closes(&node, closer);
        halted_empty_unvalued(&node, SLOT_START);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        // Three slots, each round at the slot's start.
        for slot in 0..3u64 {
            node.lock().unwrap().timestamp = SLOT_START + slot * timers().slotInterval;
            keep_one(
                &client,
                KEEPER,
                &with_closer,
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }

        assert!(
            sent::<IVault::recordValuationCall>(&node, VAULT) >= 1,
            "the Closer 'closed' the vault every slot and it was never valued: {:?}",
            everything_sent(&node)
        );
    }

    /// A Closer call that went through and left the vault open did nothing
    /// (a Closer the chain lacks, say): it isn't paid for again each slot,
    /// only once the vault's state has changed. The steps are taken
    /// meanwhile.
    #[tokio::test]
    async fn a_closer_call_that_closed_nothing_is_not_sent_again_until_the_vault_s_state_changes() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let node = node(SLOT_START);
        closes(&node, closer);
        halted_empty_unvalued(&node, SLOT_START);
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let mut slot = 0u64;
        let mut round = async |memory: &mut Memory| {
            node.lock().unwrap().timestamp = SLOT_START + slot * timers().slotInterval;
            slot += 1;
            keep_one(
                &client,
                KEEPER,
                &with_closer,
                "http://127.0.0.1:1",
                VAULT,
                memory,
            )
            .await
            .unwrap();
        };

        for _ in 0..3 {
            round(&mut memory).await;
        }
        assert_eq!(sent::<ICloser::closeHaltedCall>(&node, closer), 1);

        answers(&node, VAULT, IVault::stateCall {}, VaultState::WindingDown);
        round(&mut memory).await;
        assert_eq!(sent::<ICloser::closeHaltedCall>(&node, closer), 2);
    }

    /// The local devnet mines a block a second and moves the clock an hour a
    /// block, every block on a slot boundary: a valuation sent in any block
    /// lands at the start of a slot.
    #[tokio::test]
    async fn a_vault_on_the_devnet_clock_is_valued() {
        let now = SLOT_START;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
        let client = connect_with_blocks_of(&node, 3_600).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<IVault::recordValuationCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// While the station reads as outside the band a valuation is void and
    /// prices nothing.
    #[tokio::test]
    async fn a_valuation_that_can_only_be_void_is_not_paid_for() {
        let now = SLOT_START;
        for (outside, values) in [(true, 0), (false, 1)] {
            let node = node(now);
            an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
            answers(&node, STATION, IStation::outsideBandCall {}, outside);

            round(&node, &deployments(), "http://127.0.0.1:1").await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                values,
                "outside the band: {outside}; {:?}",
                everything_sent(&node)
            );
        }
    }

    /// The one-transaction close records a valuation after the stopped
    /// unwind: while that unwind is blocked by the band, nothing is sold, the
    /// valuation could only be void and the close would fail, so it isn't
    /// sent.
    #[tokio::test]
    async fn the_one_transaction_close_is_not_sent_while_its_valuation_would_be_void() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let now = SLOT_START;
        let node = node(now);
        closes(&node, closer);
        halted_and_empty(&node, now, now - 2 * 600);
        a_stopped_station_at(&node, 80_000, 72_000, 0, true);

        round(&node, &with_closer, "http://127.0.0.1:1").await;

        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A stopped station's unwind sells inside its own band, which widens
    /// with time, and the one-transaction close values only after it. So the
    /// close is sent whenever that unwind would sell, even while the running
    /// band reads the station as outside it: otherwise the vault would be
    /// closed step by step, which anyone can get between.
    #[tokio::test]
    async fn the_closer_is_sent_when_the_stopped_unwind_would_sell_past_the_running_band() {
        let closer = Address::repeat_byte(0x66);
        let now = SLOT_START;
        let node = node(now);
        closes(&node, closer);
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(&node, VAULT, IVault::tankCall {}, a_tank_holding(0));
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        answers(&node, STATION, IStation::outsideBandCall {}, true);
        a_stopped_station_at(&node, 80_000_000_000, 72_000_000_000, 72_000_000_000, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        stop_and_unwind(
            &client,
            KEEPER,
            "http://127.0.0.1:1",
            closer,
            VAULT,
            STATION,
            &timers(),
            0,
            1,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            1,
            "closed step by step instead: {:?}",
            everything_sent(&node)
        );
    }

    /// Anyone may collect the bill: one collected by somebody else every day
    /// doesn't put off the daily round.
    #[tokio::test]
    async fn a_bill_collected_by_someone_else_does_not_stop_the_daily_round() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        for day in 0..3u64 {
            let now = start + day * DAY;
            node.lock().unwrap().timestamp = now;
            // Somebody else collected the bill an hour before this round.
            an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }

        assert_eq!(
            sent::<IVault::refreshRestrictedCall>(&node, VAULT),
            3,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A vault cancelled before it opened has any key still approved at its
    /// station taken back, and is left alone for a day once none is.
    #[tokio::test]
    async fn a_cancelled_vault_has_its_keys_revoked() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Cancelled);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(
            &node,
            STATION,
            IStation::specCall {},
            a_spec(vec![name("local")]),
        );
        answers(
            &node,
            STATION,
            IStation::approvedKeyCall {
                driver: name("local"),
            },
            Address::repeat_byte(0x99),
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        assert_eq!(sent::<IStation::revokeKeyCall>(&node, STATION), 1);
        assert_eq!(memory.quiet_since, Some(now));
    }

    /// Inside the last bill's window a `keep` the vault refuses before it goes
    /// out (its estimate reverts, say at a moment its key's blessing has
    /// lapsed) is not the end of it: the total is tried again, and collected
    /// once it is taken.
    #[tokio::test]
    async fn a_closed_vaults_last_bill_refused_inside_its_window_is_tried_again() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 2 * DAY),
        );
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        // Halted a day ago: the last collection fell well before the grace's
        // end.
        answers(&node, VAULT, IStationVault::haltedAtCall {}, now - DAY);
        answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
        let keep = IVault::keepCall {
            total: U256::ZERO,
            asOf: 0,
            signature: Bytes::new(),
        };
        refuses(&node, VAULT, keep.clone());
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 600))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;
        // The answer is in: the vault refuses it.
        node.lock().unwrap().timestamp = now + 10;
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        assert!(latest_answer(&executor, client.chain_id, VAULT).is_some());
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);
        assert_eq!(memory.bill_sent_as_of, 0, "nothing went through");

        // Two minutes on, the node takes it.
        takes(&node, VAULT, keep);
        node.lock().unwrap().timestamp = now + 120;
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
        assert_eq!(memory.bill_sent_as_of, now - 600);
    }

    /// A closed vault with an empty tank and nothing waiting: the executor,
    /// here one that takes the connection and never answers, isn't asked
    /// for a total, and once the last bill's window is over the vault is
    /// looked at again only a day later.
    #[tokio::test]
    async fn a_closed_vault_with_nothing_owed_does_not_hold_the_round_up() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            Tank {
                balance: U256::ZERO,
                ..a_tank(now, 1_000_000, now - 2 * DAY)
            },
        );
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(&node, STATION, IStation::stoppedCall {}, true);
        answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
        // The station is stopped and empty.
        answers(
            &node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
        answers(&node, VAULT, IStationVault::haltedAtCall {}, now - DAY);
        answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let client = connect(&node).await;
        // First seen closed in the first round, its window's wall clock
        // long run: the window is judged here on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((now, a_hold_ago())),
            ..Memory::default()
        };

        for at in [now, now + LAST_ORDERS] {
            node.lock().unwrap().timestamp = at;
            let round = tokio::time::timeout(
                Duration::from_secs(3),
                keep_one(&client, KEEPER, &deployments(), &silent, VAULT, &mut memory),
            )
            .await;
            assert!(round.is_ok(), "the round waited on the executor");
        }
        assert_eq!(memory.quiet_since, Some(now + LAST_ORDERS));

        // Within the day, nothing past its state is read: a node that
        // answers nothing else leaves the round untroubled.
        let quiet = crate::stand_in::node(now + LAST_ORDERS + DAY - 1);
        answers(&quiet, VAULT, IVault::stateCall {}, VaultState::Closed);
        let client = connect(&quiet).await;
        keep_one(&client, KEEPER, &deployments(), &silent, VAULT, &mut memory)
            .await
            .unwrap();
        assert!(everything_sent(&quiet).is_empty());
    }

    #[tokio::test]
    async fn a_closed_vault_with_a_claim_still_waiting_is_not_left_alone() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            Tank {
                balance: U256::ZERO,
                ..a_tank(now, 1_000_000, now - 2 * DAY)
            },
        );
        answers(&node, STATION, IStation::stoppedCall {}, true);
        answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        a_waiting_claim(&node, now, ClaimState::Waiting);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(sent::<IVault::claimClosedCall>(&node, VAULT), 1);
        assert_eq!(memory.quiet_since, None);
    }

    /// A vault closed inside its halt's grace leaves its station running,
    /// and a running station's key can't be taken back while its blessing
    /// stands: once the grace is over the station is stopped.
    #[tokio::test]
    async fn a_vault_closed_inside_its_grace_has_its_station_stopped_once_the_grace_is_over() {
        let now = BETWEEN_SLOTS;
        for (halted_at, stops) in [(now - 60, 0), (now - 600, 1)] {
            let node = node(now);
            answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
            answers(&node, VAULT, IVault::timersCall {}, timers());
            answers(
                &node,
                VAULT,
                IVault::tankCall {},
                Tank {
                    balance: U256::ZERO,
                    ..a_tank(now, 0, now - 2 * DAY)
                },
            );
            answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
            answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
            answers(&node, VAULT, IVault::homeStationCall {}, STATION);
            answers(&node, VAULT, IStationVault::haltedAtCall {}, halted_at);
            answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
            answers(&node, STATION, IStation::stoppedCall {}, false);
            answers(
                &node,
                STATION,
                IStation::specCall {},
                a_spec(vec![name("local")]),
            );
            answers(
                &node,
                STATION,
                IStation::approvedKeyCall {
                    driver: name("local"),
                },
                Address::repeat_byte(0x99),
            );
            // The station refuses to let a standing key go while it runs.
            refuses(
                &node,
                STATION,
                IStation::revokeKeyCall {
                    driver: name("local"),
                    key: Address::repeat_byte(0x99),
                },
            );
            let client = connect(&node).await;
            let mut memory = Memory::default();

            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();

            assert_eq!(
                sent::<IStation::stop_0Call>(&node, STATION),
                stops,
                "halted at {halted_at}: {:?}",
                everything_sent(&node)
            );
            // A key still approved keeps the vault in the round.
            assert_eq!(memory.quiet_since, None);
        }
    }

    /// A moment on the keeper's clock a hold, or a closed vault's last
    /// bill's window, ago.
    fn a_hold_ago() -> std::time::Instant {
        std::time::Instant::now()
            .checked_sub(Duration::from_secs(MOST_CLOSE_HOLD))
            .expect("the clock reaches back")
    }

    fn a_bill_tank(balance: u64, settled: u64, as_of: u64, last_collection: u64) -> Tank {
        Tank {
            balance: U256::from(balance),
            keepAt: U256::ZERO,
            usageSettled: U256::from(settled),
            usageAsOf: as_of,
            lastCollection: last_collection,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        }
    }

    /// A killed vault with a stopped, empty station, nothing owed and no claims.
    fn a_halted_vault(node: &Shared, halted_at: u64, grace: u64, tank: Tank) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::tankCall {}, tank);
        answers(node, VAULT, IStationVault::haltedAtCall {}, halted_at);
        answers(node, VAULT, IStationVault::haltGraceCall {}, grace);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        answers(node, STATION, IStation::stoppedCall {}, true);
        answers(
            node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
        answers(
            node,
            STATION,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
            IStation::unwindReturn {
                sentHome: U256::ZERO,
                blocked: false,
            },
        );
    }

    /// A closed vault with nothing waiting.
    fn a_closed_vault(node: &Shared, halted_at: u64, stopped: bool, tank: Tank) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::tankCall {}, tank);
        answers(node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, VAULT, IStationVault::haltedAtCall {}, halted_at);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, STATION, IStation::stoppedCall {}, stopped);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
    }

    /// A killed vault three days past its halt, its station stopped and
    /// empty, nothing waiting, so `close` would go through; and the vault
    /// refuses every `keep` carrying the executor's newer total (the key's
    /// blessing lapsed, say), for good. Every round lands in a valuation
    /// window. The close waits for the bill, without a valuation each slot
    /// meanwhile, for no more than its longest wait in chain time and on the
    /// keeper's clock alike; then it goes, the bill uncollected.
    #[tokio::test]
    async fn a_bill_refused_for_good_holds_a_halted_vault_s_close_only_so_long() {
        let now = SLOT_START;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        refuses(
            &node,
            VAULT,
            IVault::keepCall {
                total: U256::ZERO,
                asOf: 0,
                signature: Bytes::new(),
            },
        );
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            (
                sent::<IVault::closeCall>(&node, VAULT),
                sent::<IVault::recordValuationCall>(&node, VAULT),
            )
        };

        // Held for the bill, and valued only for the close itself.
        assert_eq!(round(now, &mut memory).await, (0, 1));
        for at in (now + 10..now + MOST_CLOSE_HOLD).step_by(60) {
            assert_eq!(
                round(at, &mut memory).await,
                (0, 1),
                "{at}: {:?}",
                everything_sent(&node)
            );
        }
        // Past its longest wait in chain time, not yet on the keeper's clock.
        assert_eq!(round(now + MOST_CLOSE_HOLD, &mut memory).await, (0, 1));

        // And on the keeper's clock as well: the close goes.
        let (since, _) = memory.close_held_since.expect("the close was held");
        let long_ago = std::time::Instant::now()
            .checked_sub(Duration::from_secs(MOST_CLOSE_HOLD))
            .expect("the clock reaches back");
        memory.close_held_since = Some((since, long_ago));
        assert_eq!(round(now + MOST_CLOSE_HOLD + 10, &mut memory).await, (1, 1));
        assert_eq!(
            memory.bill_refused, 0,
            "the refused bill is counted settled"
        );
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);
    }

    /// A halted vault whose close would go through, held for its bill, and
    /// whose every `keep` carrying the executor's newer total the node
    /// refuses as the vault's `Stale`: a total signed later than the latest
    /// block, which a node behind the chain also says. That isn't the vault
    /// refusing the bill: it isn't counted refused, and the hold's last try
    /// doesn't write the bill off on it. It is tried again next round, and
    /// once the node has caught up the bill goes through and the close
    /// follows.
    #[tokio::test]
    async fn a_bill_refused_as_stale_is_not_counted_refused_and_is_tried_again() {
        let now = SLOT_START;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        let stale_keep = IVault::keepCall {
            total: U256::ZERO,
            asOf: 0,
            signature: Bytes::new(),
        };
        crate::stand_in::refuses_as(&node, VAULT, stale_keep.clone(), "Stale()");
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
            sent::<IVault::closeCall>(&node, VAULT)
        };

        assert_eq!(round(now, &mut memory).await, 0);
        // The hold is over on both clocks: its last try is refused as
        // `Stale` too, and that writes nothing off.
        memory.close_held_since = Some((now - MOST_CLOSE_HOLD, a_hold_ago()));
        assert_eq!(
            round(now + 10, &mut memory).await,
            0,
            "the bill was written off on a `Stale` the keeper's node may only say for being behind"
        );
        assert_eq!(
            memory.bill_refused, 0,
            "a `keep` refused as `Stale` was counted as the vault refusing the bill"
        );

        takes(&node, VAULT, stale_keep);
        assert_eq!(
            round(now + 20, &mut memory).await,
            1,
            "{:?}",
            everything_sent(&node)
        );
        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "the bill refused as `Stale` was not tried again"
        );
    }

    /// A halted vault's close held for its bill, the hold over and its last
    /// try already made once without reaching the vault; then the executor
    /// gives no answer at all, which lets the close go. That settles the
    /// bill: the hold and its last try both end, so a later hold has a last
    /// try of its own.
    #[tokio::test]
    async fn a_close_let_go_with_no_answer_ends_the_hold_s_last_try_too() {
        let now = SLOT_START;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        let executor = failing_executor(1_000, 1_200_000, grace_end + 300).await;
        let client = connect(&node).await;
        let mut memory = Memory {
            close_held_since: Some((now - MOST_CLOSE_HOLD, a_hold_ago())),
            last_try_since: Some((now - 60, std::time::Instant::now())),
            ..Memory::default()
        };

        assert!(
            collect_before_close(
                &client,
                KEEPER,
                &executor,
                VAULT,
                grace_end,
                now,
                timers().slotInterval,
                true,
                &mut memory,
            )
            .await,
            "setup: no answer at all did not let the close go"
        );
        assert_eq!(memory.close_held_since, None);
        assert_eq!(
            memory.last_try_since, None,
            "the bill settled with no answer, and the hold's last try was left running"
        );
    }

    /// A halted vault's close held for its bill, the hold over, and every
    /// `keep` carrying the executor's total mined and reverted. The round's
    /// own `keep` for the total goes first and reverts: that was the total's
    /// try, so the hold's last try doesn't pay for another revert in the same
    /// round, and the bill is written off.
    #[tokio::test]
    async fn a_total_reverted_earlier_in_the_round_is_not_sent_again_as_its_last_try() {
        let now = SLOT_START;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        ask(
            node.lock().unwrap().chain_id,
            &executor,
            VAULT,
            Lane::Halted,
        );
        asks_back(&executor).await;
        let client =
            crate::stand_in::connect_reverting(&node, Arc::new(AtomicBool::new(true))).await;
        let mut memory = Memory {
            close_held_since: Some((now - MOST_CLOSE_HOLD, a_hold_ago())),
            ..Memory::default()
        };

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;
        let billed = crate::stand_in::sent_calls::<IVault::keepCall>(&node, VAULT)
            .iter()
            .filter(|keep| !keep.total.is_zero())
            .count();
        assert_eq!(
            billed,
            1,
            "the total mined and reverted this round was paid for again as the last try: {:?}",
            everything_sent(&node)
        );
        assert_eq!(
            memory.written_off_for,
            Some(grace_end),
            "the bill was not written off"
        );
    }

    /// A halted vault's close held for a bill the vault refuses for good, the
    /// hold over, and the Closer able to close it. The Closer's path makes the
    /// last try, the vault refuses it, the bill is written off and the Closer
    /// goes; but its call leaves the vault open, so `close` is tried in the
    /// same round. By then the executor has signed a newer total, which the
    /// vault refuses too: it is tried, but it doesn't hold the close again for
    /// the same halt, and `close` goes in that round.
    #[tokio::test]
    async fn a_newer_total_after_a_bill_written_off_does_not_hold_the_close_again() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let now = SLOT_START;
        let node = node(now);
        halted_and_empty(&node, now, now - 2 * 600);
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_tank(now, 1_000_000, now - 3_600),
        );
        closes(&node, closer);
        refuses(
            &node,
            VAULT,
            IVault::keepCall {
                total: U256::ZERO,
                asOf: 0,
                signature: Bytes::new(),
            },
        );
        let total = Arc::new(AtomicU64::new(1_500_000));
        let as_of = Arc::new(AtomicU64::new(now - 60));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        // The Closer's call goes through and leaves the vault open; the
        // executor signs a newer total meanwhile.
        let hook: Hook = Arc::new(move |_, to, _| {
            if to == closer {
                total.store(1_700_000, Ordering::SeqCst);
                as_of.store(now - 10, Ordering::SeqCst);
            }
        });
        let client = connect_with_hook(&node, hook).await;
        let mut memory = Memory {
            asked_at_halt: true,
            asked_after_grace: true,
            close_held_since: Some((now - MOST_CLOSE_HOLD, a_hold_ago())),
            ..Memory::default()
        };

        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            1,
            "setup: the Closer did not go once the bill was written off: {:?}",
            everything_sent(&node)
        );
        assert_eq!(
            sent::<IVault::closeCall>(&node, VAULT),
            1,
            "a newer total held the close again after its bill was written off: {:?}",
            everything_sent(&node)
        );
    }

    /// An open vault whose `keep` carrying its bill passes its estimate and
    /// is mined and reverts, every time. Each paid its gas and collected
    /// nothing, so it is not sent every round: next slot, then two on, four
    /// and so on, up to an hour apart. Once one goes through, the next that
    /// reverts waits a slot again.
    #[tokio::test]
    async fn a_keep_mined_and_reverted_every_time_is_sent_less_and_less_often() {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
        let total = Arc::new(AtomicU64::new(1_500_000));
        let as_of = Arc::new(AtomicU64::new(now - 600));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        let reverting = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_reverting(&node, reverting.clone()).await;
        let mut memory = Memory::default();
        let billed = || {
            crate::stand_in::sent_calls::<IVault::keepCall>(&node, VAULT)
                .iter()
                .filter(|keep| !keep.total.is_zero())
                .count()
        };
        // A round in each of these slots, counted from `now`: the slots a
        // `keep` carrying the bill went out in.
        let rounds = async |slots: std::ops::RangeInclusive<u64>, memory: &mut Memory| {
            let mut sent_in = Vec::new();
            for slot in slots {
                let before = billed();
                node.lock().unwrap().timestamp = now + slot * interval;
                keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                    .await
                    .unwrap();
                asks_back(&executor).await;
                if billed() > before {
                    sent_in.push(slot);
                }
            }
            sent_in
        };

        // The daily ask goes out in the first round; its answer is collected
        // from the next.
        let sent_in = rounds(0..=40, &mut memory).await;
        assert_eq!(
            sent_in,
            [1, 2, 4, 8, 16, 32],
            "{:?}",
            everything_sent(&node)
        );
        assert_eq!(memory.bill_sent_as_of, 0, "nothing went through");
        assert!(
            BILLS_REFUSED
                .lock()
                .unwrap()
                .contains_key(&(client.chain_id, VAULT)),
            "the bill waiting on a refused keep is no longer heard of"
        );

        // It goes through when next due, which starts the count again.
        reverting.store(false, Ordering::SeqCst);
        assert_eq!(rounds(41..=64, &mut memory).await, [64]);
        assert_eq!(memory.bill_sent_as_of, now - 600);
        assert_eq!(memory.keep_again, None);

        // A newer total that reverts again waits one slot, not where the
        // count had reached.
        reverting.store(true, Ordering::SeqCst);
        total.store(2_000_000, Ordering::SeqCst);
        as_of.store(now, Ordering::SeqCst);
        ask(client.chain_id, &executor, VAULT, Lane::Daily);
        asks_back(&executor).await;
        assert_eq!(rounds(65..=68, &mut memory).await, [65, 66, 68]);

        // The wait stops growing at an hour.
        let mut again = None;
        for _ in 0..20 {
            again = Some(send_again(again.as_ref(), now, interval, 0));
        }
        assert_eq!(
            again.map(|again| again.wait * interval),
            Some(MOST_SECONDS_BETWEEN_REVERTS)
        );
    }

    /// An open vault whose `keep` carrying its bill is mined and reverts,
    /// every time, while the executor answers with a newer total every
    /// slot. The wait between paid `keep`s is the vault's, not each total's:
    /// a newer total waits it out too, so the stream of totals doesn't have
    /// every round pay for a `keep` that reverts.
    #[tokio::test]
    async fn a_stream_of_newer_totals_does_not_have_every_round_pay_for_a_reverted_keep() {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let node = node(now);
        an_open_vault(&node, a_tank(now, 1_000_000, now - 3_600));
        let total = Arc::new(AtomicU64::new(1_500_000));
        let as_of = Arc::new(AtomicU64::new(now - 600));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        let client =
            crate::stand_in::connect_reverting(&node, Arc::new(AtomicBool::new(true))).await;
        let mut memory = Memory::default();
        let billed = || {
            crate::stand_in::sent_calls::<IVault::keepCall>(&node, VAULT)
                .iter()
                .filter(|keep| !keep.total.is_zero())
                .count()
        };
        let mut sent_in = Vec::new();
        for slot in 0..=40 {
            // A newer total, answered before the slot's round.
            total.store(1_500_000 + slot * 1_000, Ordering::SeqCst);
            as_of.store(now - 600 + slot * interval, Ordering::SeqCst);
            ask(client.chain_id, &executor, VAULT, Lane::Daily);
            asks_back(&executor).await;
            let before = billed();
            node.lock().unwrap().timestamp = now + slot * interval;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            if billed() > before {
                sent_in.push(slot);
            }
        }
        assert_eq!(
            sent_in,
            [0, 1, 3, 7, 15, 31],
            "a newer total was sent before the vault's wait was over: {:?}",
            everything_sent(&node)
        );
    }

    /// A closed vault in its last bill's window whose `keep`s carrying its
    /// total are mined and revert for the first eleven minutes, so the total
    /// waits ever longer between tries, the last wait reaching past the
    /// window's end. Whatever reverted them has passed by then. Before the
    /// vault goes quiet the total is tried once more, and collected.
    #[tokio::test]
    async fn a_closed_vault_s_total_waiting_after_reverts_is_tried_once_more_before_it_goes_quiet()
    {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_finished_closed_vault(
            &node,
            halted_at,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 5))).await;
        let reverting = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_reverting(&node, reverting.clone()).await;
        // First seen closed now, the window judged on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((now, a_hold_ago())),
            ..Memory::default()
        };
        let mut slot = 0;
        while memory.quiet_since.is_none() && slot < 3 * LAST_ORDERS / interval {
            let at = now + slot * interval;
            reverting.store(at < now + 660, Ordering::SeqCst);
            node.lock().unwrap().timestamp = at;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
            slot += 1;
        }
        assert!(
            memory.quiet_since.is_some(),
            "setup: the vault never went quiet"
        );
        let tried_after_reverts = sent_at::<IVault::keepCall>(&node, VAULT)
            .into_iter()
            .filter(|at| *at >= now + 660)
            .count();
        assert_eq!(
            tried_after_reverts,
            1,
            "the vault went quiet with its total never tried once its keeps stopped reverting; \
             sent at: {:?}",
            sent_at::<IVault::keepCall>(&node, VAULT)
        );
    }

    /// A closed vault past its last bill's window that stays in the round
    /// (its station still runs), whose `keep`s carrying a total are mined and
    /// revert every time, while the executor answers with a newer total every
    /// slot. The window's end has one last try; after it the totals wait out
    /// the vault's wait as any other does, so the stream of totals doesn't
    /// have every round pay for a `keep` that reverts.
    #[tokio::test]
    async fn a_closed_vault_s_last_try_is_made_once_at_its_window_s_end() {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        // A vault of its own: the refused bills are counted by vault.
        let vault = Address::repeat_byte(0x5c);
        answers(&node, vault, IVault::stateCall {}, VaultState::Closed);
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(
            &node,
            vault,
            IVault::tankCall {},
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, vault, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IStationVault::haltedAtCall {}, halted_at);
        answers(&node, vault, IStationVault::haltGraceCall {}, 600);
        answers(&node, STATION, IStation::stoppedCall {}, false);
        answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
        let total = Arc::new(AtomicU64::new(1_200_000));
        let as_of = Arc::new(AtomicU64::new(grace_end + 5));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        let client =
            crate::stand_in::connect_reverting(&node, Arc::new(AtomicBool::new(true))).await;
        // Seen closed a day ago: the window is long over.
        let mut memory = Memory {
            closed_seen_at: Some((now - DAY, a_hold_ago())),
            ..Memory::default()
        };
        let billed = || {
            crate::stand_in::sent_calls::<IVault::keepCall>(&node, vault)
                .iter()
                .filter(|keep| !keep.total.is_zero())
                .count()
        };
        let mut sent_in = Vec::new();
        for slot in 0..=40 {
            // A newer total, answered before the slot's round.
            total.store(1_200_000 + slot * 1_000, Ordering::SeqCst);
            as_of.store(grace_end + 5 + slot * interval, Ordering::SeqCst);
            ask(client.chain_id, &executor, vault, Lane::Halted);
            asks_back(&executor).await;
            let before = billed();
            node.lock().unwrap().timestamp = now + slot * interval;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                vault,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
            if billed() > before {
                sent_in.push(slot);
            }
        }
        assert_eq!(memory.quiet_since, None, "setup: the vault went quiet");
        assert_eq!(
            sent_in,
            [0, 1, 3, 7, 15, 31],
            "the window's last try was made again every round: {:?}",
            everything_sent(&node)
        );
    }

    /// A halted vault whose close would go through and whose `keep`s carrying
    /// its bill are mined and revert for the first eleven minutes, so the
    /// bill waits ever longer between tries, the last wait reaching past the
    /// hold's end. Whatever reverted them has passed by then. Before the
    /// close goes the bill is tried once more, and collected.
    #[tokio::test]
    async fn a_halted_vault_s_bill_waiting_after_reverts_is_tried_once_more_before_the_close_goes()
    {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        let reverting = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_reverting(&node, reverting.clone()).await;
        let mut memory = Memory::default();
        let mut slot = 0;
        while sent::<IVault::closeCall>(&node, VAULT) == 0 && slot < 3 * MOST_CLOSE_HOLD / interval
        {
            let at = now + slot * interval;
            reverting.store(at < now + 660, Ordering::SeqCst);
            node.lock().unwrap().timestamp = at;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
            // The hold is judged on the chain's clock.
            if let Some((since, _)) = memory.close_held_since {
                memory.close_held_since = Some((since, a_hold_ago()));
            }
            slot += 1;
        }
        assert_eq!(
            sent::<IVault::closeCall>(&node, VAULT),
            1,
            "setup: never closed"
        );
        let tried_after_reverts = sent_at::<IVault::keepCall>(&node, VAULT)
            .into_iter()
            .filter(|at| *at >= now + 660)
            .count();
        assert_eq!(
            tried_after_reverts,
            1,
            "the close went with the bill never tried once its keeps stopped reverting; sent \
             at: {:?}",
            sent_at::<IVault::keepCall>(&node, VAULT)
        );
    }

    /// A halted vault whose close would go through holds it while its bill
    /// is turned away. The hold ends only when the bill settles: a round
    /// that finds the close would not go through leaves it running, and a
    /// close found possible again is still measured from the first hold.
    #[tokio::test]
    async fn a_close_hold_ends_only_when_its_bill_settles() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        let close = IVault::closeCall {};
        answers(&node, VAULT, close.clone(), IVault::closeReturn {});
        let keep = IVault::keepCall {
            total: U256::ZERO,
            asOf: 0,
            signature: Bytes::new(),
        };
        refuses(&node, VAULT, keep.clone());
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
        };

        round(now, &mut memory).await;
        let (since, _) = memory
            .close_held_since
            .expect("setup: the close was not held");
        crate::stand_in::reverts(&node, VAULT, close.clone(), "NotYet()");
        round(now + 10, &mut memory).await;
        assert_eq!(
            memory.close_held_since.map(|(at, _)| at),
            Some(since),
            "a round that found the close would not go through ended its hold"
        );

        answers(&node, VAULT, close, IVault::closeReturn {});
        round(now + 20, &mut memory).await;
        assert_eq!(
            memory.close_held_since.map(|(at, _)| at),
            Some(since),
            "the hold started again"
        );
        takes(&node, VAULT, keep);
        round(now + 30, &mut memory).await;
        assert_eq!(sent::<IVault::closeCall>(&node, VAULT), 1);
        assert_eq!(memory.close_held_since, None, "the bill went through");
    }

    /// A halted vault whose bill the vault refuses for good: its close is
    /// held for the bill, the refusal counted once a round, however often the
    /// round tries the bill, until the hold is over and the close goes, the
    /// bill counted settled. In the closed vault's last bill's window after,
    /// that bill is not said refused again.
    #[tokio::test]
    async fn a_bill_counted_settled_when_the_close_goes_is_not_said_refused_again_once_closed() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_halted_vault(
            &node,
            halted_at,
            600,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        refuses(
            &node,
            VAULT,
            IVault::keepCall {
                total: U256::ZERO,
                asOf: 0,
                signature: Bytes::new(),
            },
        );
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 300))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let round = async |at: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &deployments(), &executor, VAULT, memory)
                .await
                .unwrap();
            asks_back(&executor).await;
        };

        // The close waits for the executor's answer in the first round, and
        // the bill is refused from then on.
        for rounds in 0..5u64 {
            round(now + rounds * 60, &mut memory).await;
            assert_eq!(
                memory.bill_refused,
                rounds + 1,
                "counted more than once a round"
            );
        }
        let (since, _) = memory.close_held_since.expect("the close was held");
        memory.close_held_since = Some((since, a_hold_ago()));
        round(since + MOST_CLOSE_HOLD, &mut memory).await;
        assert_eq!(
            sent::<IVault::closeCall>(&node, VAULT),
            1,
            "setup: never closed"
        );
        assert_eq!(memory.bill_refused, 0);

        answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
        for minutes in 1..=5 {
            round(since + MOST_CLOSE_HOLD + minutes * 60, &mut memory).await;
            assert_eq!(
                memory.bill_refused, 0,
                "the bill counted settled is said refused again in the closed vault's window"
            );
        }
    }

    /// A closed vault with nothing owed goes quiet only once its last bill's
    /// window is over on the keeper's own clock as well as the chain's: a
    /// chain whose clock runs fast (the devnet moves an hour a block) would
    /// otherwise leave the window no time at all.
    #[tokio::test]
    async fn a_closed_vault_s_last_bill_window_is_not_cut_short_by_a_fast_chain_clock() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_finished_closed_vault(
            &node,
            now - DAY,
            Tank {
                balance: U256::ZERO,
                ..a_tank(now, 1_000_000, now - 2 * DAY)
            },
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();
        for at in [now, now + 3_600, now + 2 * 3_600] {
            node.lock().unwrap().timestamp = at;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            memory.quiet_since, None,
            "the window was over in no time on the keeper's clock"
        );

        let (seen, _) = memory.closed_seen_at.expect("seen closed");
        memory.closed_seen_at = Some((seen, a_hold_ago()));
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        assert_eq!(memory.quiet_since, Some(now + 2 * 3_600));
    }

    /// A vault that has refused its valuation slot after slot is no longer
    /// said to once the keeper no longer wants one (its close is held for its
    /// bill, and has the valuation it needs).
    #[tokio::test]
    async fn a_valuations_alarm_clears_once_no_valuation_is_wanted() {
        // A vault of its own: the health is the whole keeper's.
        let vault = Address::repeat_byte(0x7e);
        let node = node(SLOT_START);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        refuses(&node, vault, IVault::recordValuationCall {});
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let heard_of = || {
            CALLS_REFUSED.lock().unwrap().contains_key(&(
                client.chain_id,
                vault,
                Refused::Valuations,
            ))
        };
        for n in 0..MOST_SLOTS_REFUSED {
            node.lock().unwrap().timestamp = SLOT_START + 10 * n;
            price(
                &client,
                KEEPER,
                vault,
                &timers(),
                1,
                true,
                Halt::Running,
                &mut memory,
            )
            .await;
        }
        assert!(heard_of(), "setup: the refusals were not heard of");

        node.lock().unwrap().timestamp = SLOT_START + 10 * MOST_SLOTS_REFUSED;
        price(
            &client,
            KEEPER,
            vault,
            &timers(),
            1,
            false,
            Halt::Running,
            &mut memory,
        )
        .await;
        assert!(
            !heard_of(),
            "no valuation is wanted, and the vault is still said to refuse them"
        );
        assert_eq!(memory.valuations_refused.slots, 0);
    }

    /// A vault that refuses its valuation before it goes out, slot after
    /// slot, is in the keeper's health once it has for ten slots in a row,
    /// and is no longer once one goes through.
    #[tokio::test]
    async fn a_vault_that_keeps_refusing_its_valuation_is_heard_of_until_one_goes_through() {
        // A vault of its own: the health is the whole keeper's.
        let vault = Address::repeat_byte(0x7d);
        let node = node(SLOT_START);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        refuses(&node, vault, IVault::recordValuationCall {});
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let heard_of = || {
            CALLS_REFUSED
                .lock()
                .unwrap()
                .get(&(client.chain_id, vault, Refused::Valuations))
                .copied()
        };
        let slot = async |n: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = SLOT_START + 10 * n;
            price(
                &client,
                KEEPER,
                vault,
                &timers(),
                1,
                true,
                Halt::Running,
                memory,
            )
            .await;
        };

        for n in 0..MOST_SLOTS_REFUSED - 1 {
            slot(n, &mut memory).await;
        }
        assert_eq!(heard_of(), None, "a few refusals are ordinary");
        slot(MOST_SLOTS_REFUSED - 1, &mut memory).await;
        assert_eq!(heard_of(), Some(MOST_SLOTS_REFUSED));

        takes(&node, vault, IVault::recordValuationCall {});
        slot(MOST_SLOTS_REFUSED, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, vault), 1);
        assert_eq!(heard_of(), None);
    }

    /// The bill collected before the one-transaction close may take a block of
    /// its own, so whether the close still lands inside the valuation window is
    /// judged on the clock after it: one landing outside would have its
    /// valuation, and the whole close with it, reverted.
    #[tokio::test]
    async fn the_one_transaction_close_is_judged_on_the_clock_after_the_bill_sent_before_it() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        // The grace ends at SLOT_START + 15. With the head at the slot's
        // window's start the bill and the close both land inside it; two
        // seconds in, the bill takes the window's last second.
        for (head, closed) in [(SLOT_START + 20, 1), (SLOT_START + 22, 0)] {
            let grace_end = SLOT_START + 15;
            let collected = SLOT_START - 120;
            let first = SLOT_START + 10;
            let node = node(first);
            closes(&node, closer);
            a_halted_vault(
                &node,
                grace_end - 600,
                600,
                a_bill_tank(50_000_000, 1_000_000, collected, first - 3_600),
            );
            reverts(&node, VAULT, IVault::closeCall {}, "not yet");
            let as_of = Arc::new(AtomicU64::new(collected));
            let executor = executor(1_500_000, as_of.clone()).await;
            let client = connect_with_blocks_of(&node, 1).await;
            let mut memory = Memory::default();

            // Inside the grace: the executor's total is the one already
            // collected.
            keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
                .await
                .unwrap();
            asks_back(&executor).await;

            // The grace is over and the executor has signed its last total.
            as_of.store(grace_end + 1, Ordering::SeqCst);
            node.lock().unwrap().timestamp = head;
            keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
                .await
                .unwrap();
            asks_back(&executor).await;

            assert_eq!(
                sent::<IVault::keepCall>(&node, VAULT),
                1,
                "the last bill was not collected first; sent: {:?}",
                everything_sent(&node)
            );
            let window = timers().slotWindow;
            let interval = timers().slotInterval;
            let landed: Vec<u64> = sent_at::<ICloser::closeHaltedCall>(&node, closer)
                .into_iter()
                .map(|at| at + 1)
                .collect();
            assert!(
                landed
                    .iter()
                    .all(|at| at - slot_of(*at, interval) <= window),
                "the close lands outside the valuation window at {landed:?}; sent: {:?}",
                everything_sent(&node)
            );
            assert_eq!(
                landed.len(),
                closed,
                "head at {head}: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// Once a collection has reached the halt's grace end and left no cap
    /// carried, the vault charges nothing more and only writes a newer total
    /// off, so a closed vault isn't sent a `keep` for each newer total the
    /// executor signs.
    #[tokio::test]
    async fn a_closed_vault_is_not_sent_a_bill_its_grace_has_already_capped() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let halted_at = now - 1_200;
        let grace_end = halted_at + 600;
        // The last collection reached the grace's end.
        a_closed_vault(
            &node,
            halted_at,
            true,
            a_bill_tank(50_000_000, 1_000_000, grace_end, grace_end),
        );
        // The executor signs a later, slightly higher total.
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(now - 30))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            0,
            "a keep that can charge nothing was paid for: {:?}",
            everything_sent(&node)
        );
    }

    /// The tank after a collection that reached the grace's end and left
    /// $3 of cap carried: the vault still charges a newer total up to that.
    fn a_tank_carrying_cap_past_the_grace(grace_end: u64) -> Tank {
        Tank {
            capCarried: U256::from(3_000_000u64),
            ..a_bill_tank(50_000_000, 1_000_000, grace_end + 5, grace_end)
        }
    }

    /// Past the grace a vault still charges a newer total out of the cap it
    /// carries: the gas of the agent's last orders, sent in the grace and
    /// landing after the first collection past it, is collected before the
    /// vault closes.
    #[tokio::test]
    async fn a_halted_vaults_later_total_is_collected_before_its_close_while_it_carries_cap() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let halted_at = now - 1_200;
        let grace_end = halted_at + 600;
        a_halted_vault(
            &node,
            halted_at,
            600,
            a_tank_carrying_cap_past_the_grace(grace_end),
        );
        answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 100))).await;
        let client = connect(&node).await;
        // The halt's bill and the first one past the grace were collected.
        let mut memory = Memory {
            asked_at_halt: true,
            asked_after_grace: true,
            bill_sent_as_of: grace_end + 5,
            daily_at: Some(now - 60),
            ..Memory::default()
        };

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// The same once the vault has closed, whose author may take the whole
    /// tank at once: its last bill is collected before it is left quiet.
    #[tokio::test]
    async fn a_closed_vaults_later_total_is_collected_while_it_carries_cap() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let halted_at = now - 1_200;
        let grace_end = halted_at + 600;
        a_closed_vault(
            &node,
            halted_at,
            true,
            a_tank_carrying_cap_past_the_grace(grace_end),
        );
        answers(
            &node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 100))).await;
        let client = connect(&node).await;
        let mut memory = Memory {
            bill_sent_as_of: grace_end + 5,
            ..Memory::default()
        };

        for seconds in [0, 10] {
            node.lock().unwrap().timestamp = now + seconds;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A closed vault still carrying cap, everything else done, whose
    /// executor answers that it serves no total for it (an executor started
    /// again serves no closed vault): once the last bill's window is over
    /// the vault goes quiet rather than being read in full, and the executor
    /// asked, every round for good. It is still looked at a day later, and a
    /// total the executor serves by then is collected.
    #[tokio::test]
    async fn a_closed_vault_goes_quiet_when_no_executor_serves_its_total() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_closed_vault(
            &node,
            halted_at,
            true,
            a_tank_carrying_cap_past_the_grace(grace_end),
        );
        answers(
            &node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
        let client = connect(&node).await;
        let serving_nothing = executor_serving_nothing().await;
        // First seen closed in the first round, its window's wall clock
        // long run: the window is judged here on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((now, a_hold_ago())),
            ..Memory::default()
        };
        for minutes in 0..20u64 {
            node.lock().unwrap().timestamp = now + minutes * 61;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &serving_nothing,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&serving_nothing).await;
        }
        // The first round after the executor answered an ask sent at least
        // the window's length after the vault was first seen closed.
        assert_eq!(
            memory.quiet_since,
            Some(now + 16 * 61),
            "a closed vault with its last bill collected is held in every round for an executor that serves no total"
        );

        // A day on it is looked at again, and the executor asked.
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 100))).await;
        for seconds in [0, 61] {
            node.lock().unwrap().timestamp = now + 16 * 61 + DAY + seconds;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }
        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A closed vault with a stopped, empty station and nothing waiting:
    /// only its last bill keeps it in the round.
    fn a_finished_closed_vault(node: &Shared, halted_at: u64, tank: Tank) {
        a_closed_vault(node, halted_at, true, tank);
        answers(
            node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::ZERO,
                exit: U256::ZERO,
            },
        );
    }

    /// A failed ask of the executor (a 503, a timeout, a refused
    /// connection) says nothing about whether it has a total, so a closed
    /// vault whose grace's usage is still owed is not left quiet for a day
    /// on it: the executor is asked again a minute later and the total
    /// collected.
    #[tokio::test]
    async fn a_closed_vault_whose_one_executor_ask_fails_is_not_left_quiet_for_a_day() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 700;
        let grace_end = halted_at + 600;
        let node = node(now);
        // Last collected when the halt was first seen.
        a_finished_closed_vault(
            &node,
            halted_at,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, halted_at + 2, halted_at + 2)
            },
        );
        let executor = failing_executor(1, 1_400_000, grace_end + 5).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        for minutes in 0..5u64 {
            node.lock().unwrap().timestamp = now + minutes * 61;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "one failed ask left the last bill uncollected (quiet since {:?}); sent: {:?}",
            memory.quiet_since,
            everything_sent(&node)
        );
    }

    /// The executor's meter signs a few seconds after the first round that
    /// finds the vault closed, which found the total it served already
    /// collected. The vault stays in the round through the last bill's
    /// window, so the newer total is collected at the next ask.
    #[tokio::test]
    async fn a_total_signed_a_few_seconds_after_the_first_closed_round_is_still_collected() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 700;
        let grace_end = halted_at + 600;
        let node = node(now);
        // The Closer's round collected 1_500_000 as of just past the grace.
        a_finished_closed_vault(
            &node,
            halted_at,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_500_000, grace_end + 1, grace_end)
            },
        );
        let total = Arc::new(AtomicU64::new(1_500_000));
        let as_of = Arc::new(AtomicU64::new(grace_end + 1));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        let client = connect(&node).await;
        let mut memory = Memory {
            bill_sent_as_of: grace_end + 1,
            asked_at_halt: true,
            asked_after_grace: true,
            ..Memory::default()
        };

        // The first round that finds the vault closed: nothing new.
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;
        assert_eq!(sent::<IVault::keepCall>(&node, VAULT), 0);

        // Two seconds on the meter signs the agent's last orders' gas.
        total.store(1_600_000, Ordering::SeqCst);
        as_of.store(now + 2, Ordering::SeqCst);
        for seconds in [3u64, 61, 122, 600] {
            node.lock().unwrap().timestamp = now + seconds;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "the total signed just after the first closed round was not collected \
             (quiet since {:?}); sent: {:?}",
            memory.quiet_since,
            everything_sent(&node)
        );
    }

    /// A closed vault whose executor still serves a total the vault won't
    /// take (the key's blessing lapsed after the executor let the vault go,
    /// so every `keep` is refused): once the last bill's window is over the
    /// refusal settles the bill and the vault goes quiet, rather than being
    /// read in full every round for good.
    #[tokio::test]
    async fn a_closed_vault_whose_last_total_the_vault_refuses_goes_quiet_in_the_end() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let grace_end = halted_at + 600;
        let node = node(now);
        a_finished_closed_vault(
            &node,
            halted_at,
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_000_000, grace_end - 10, grace_end - 10)
            },
        );
        refuses(
            &node,
            VAULT,
            IVault::keepCall {
                total: U256::ZERO,
                asOf: 0,
                signature: Bytes::new(),
            },
        );
        let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 5))).await;
        let client = connect(&node).await;
        // First seen closed in the first round, its window's wall clock
        // long run: the window is judged here on the chain's clock.
        let mut memory = Memory {
            closed_seen_at: Some((now, a_hold_ago())),
            ..Memory::default()
        };

        // Two days of rounds, an hour apart.
        for hours in 0..48u64 {
            node.lock().unwrap().timestamp = now + hours * 3_600;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }

        assert!(
            memory.quiet_since.is_some(),
            "a closed vault whose last total can never be collected is read in full every round for good"
        );
    }

    /// Only the vault reverting a `keep` settles a closed vault's last bill.
    /// A node that turns the `keep` away for its own reasons (the keeper's
    /// account can't pay for gas, a rate limit, a fee under the base fee, a
    /// gateway error in a JSON body) said nothing about the vault, so the
    /// bill is not collected and the vault stays in the round: once the
    /// node takes sends again, the owed total is collected.
    #[tokio::test]
    async fn a_keep_the_node_turned_away_for_its_own_reasons_does_not_settle_a_closed_vaults_bill()
    {
        for refusal in [
            serde_json::json!({"code": -32000, "message": "insufficient funds for gas * price + value: balance 0, tx cost 21000000000000"}),
            serde_json::json!({"code": -32005, "message": "request rate limit exceeded"}),
            serde_json::json!({"code": -32000, "message": "max fee per gas less than block base fee"}),
            serde_json::json!({"code": -32603, "message": "502 Bad Gateway"}),
        ] {
            let now = BETWEEN_SLOTS;
            let halted_at = now - 3 * DAY;
            let grace_end = halted_at + 600;
            let node = node(now);
            a_finished_closed_vault(
                &node,
                halted_at,
                Tank {
                    capCarried: U256::from(3_000_000u64),
                    ..a_bill_tank(50_000_000, 1_000_000, grace_end + 5, grace_end)
                },
            );
            let executor = executor(1_200_000, Arc::new(AtomicU64::new(grace_end + 100))).await;
            let turning_away = Arc::new(AtomicBool::new(true));
            let client = crate::stand_in::connect_turning_away::<IVault::keepCall>(
                &node,
                refusal.clone(),
                turning_away.clone(),
            )
            .await;
            let mut memory = Memory {
                bill_sent_as_of: grace_end + 5,
                // First seen closed long ago: the last-orders window is over.
                closed_seen_at: Some((grace_end, a_hold_ago())),
                ..Memory::default()
            };

            // The node turns the keeper's `keep` away this round.
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
            assert_eq!(memory.quiet_since, None, "settled on {refusal}");

            // It takes sends again; rounds a minute apart.
            turning_away.store(false, Ordering::SeqCst);
            for minutes in 1..=3u64 {
                node.lock().unwrap().timestamp = now + minutes * 61;
                keep_one(
                    &client,
                    KEEPER,
                    &deployments(),
                    &executor,
                    VAULT,
                    &mut memory,
                )
                .await
                .unwrap();
                asks_back(&executor).await;
            }

            assert_eq!(
                sent::<IVault::keepCall>(&node, VAULT),
                1,
                "the owed total was not collected after the node turned a keep away with \
                 {refusal}; sent: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// An executor that can't be reached says nothing about whether it has
    /// a total, so a closed vault is kept in the round for it, past the last
    /// bill's window, rather than left quiet for a day.
    #[tokio::test]
    async fn a_closed_vault_is_not_left_quiet_while_its_executor_cannot_be_reached() {
        let now = BETWEEN_SLOTS;
        let halted_at = now - 3 * DAY;
        let node = node(now);
        a_finished_closed_vault(
            &node,
            halted_at,
            a_tank_carrying_cap_past_the_grace(halted_at + 600),
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        for minutes in 0..30u64 {
            node.lock().unwrap().timestamp = now + minutes * 61;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
        }

        assert_eq!(memory.quiet_since, None);
    }

    /// The Closer closes a halted vault in the first window round past the
    /// grace, collecting with the total asked at that round's start. The
    /// agent's last orders land a block or two later and the executor signs
    /// them; the cap the vault carries would charge them. The closed vault
    /// asks the executor again once its minute is up and collects that total.
    #[tokio::test]
    async fn the_total_signed_just_after_the_closer_closed_the_vault_is_collected() {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let grace_end = SLOT_START + 15;
        let halted_at = grace_end - 600;
        let first = BETWEEN_SLOTS;
        let node = node(first);
        closes(&node, closer);
        a_halted_vault(
            &node,
            halted_at,
            600,
            a_bill_tank(50_000_000, 1_000_000, first - 120, first - 120),
        );
        answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        let hook: Hook = Arc::new(move |node, to, _| {
            if to == closer {
                answers(node, VAULT, IVault::stateCall {}, VaultState::Closed);
            }
        });
        let client = connect_with_hook(&node, hook).await;
        let total = Arc::new(AtomicU64::new(1_000_000));
        let as_of = Arc::new(AtomicU64::new(first - 120));
        let executor = crate::stand_in::moving_executor(total.clone(), as_of.clone()).await;
        let mut memory = Memory::default();

        // Inside the grace: nothing new.
        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;

        // The grace is over: the executor's total as of the grace's end is
        // collected and the Closer closes the vault.
        total.store(1_500_000, Ordering::SeqCst);
        as_of.store(grace_end + 1, Ordering::SeqCst);
        node.lock().unwrap().timestamp = SLOT_START + 20;
        keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
            .await
            .unwrap();
        asks_back(&executor).await;
        assert_eq!(
            sent::<ICloser::closeHaltedCall>(&node, closer),
            1,
            "{:?}",
            everything_sent(&node)
        );
        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            Tank {
                capCarried: U256::from(3_000_000u64),
                ..a_bill_tank(50_000_000, 1_500_000, grace_end + 1, grace_end)
            },
        );

        // Two seconds on, the agent's last order has landed and the executor
        // has signed it. The vault is kept in the round, and the executor
        // asked again a minute after it last was.
        total.store(1_600_000, Ordering::SeqCst);
        as_of.store(SLOT_START + 21, Ordering::SeqCst);
        for at in [SLOT_START + 22, SLOT_START + 81] {
            node.lock().unwrap().timestamp = at;
            keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
                .await
                .unwrap();
            asks_back(&executor).await;
        }

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            2,
            "the closed vault's last total was not collected: {:?}",
            everything_sent(&node)
        );
        assert_eq!(memory.quiet_since, None);
    }

    /// From an empty tank a collection charges nothing and writes the whole bill
    /// off, so a later top-up could never pay it: the daily round doesn't send
    /// one.
    #[tokio::test]
    async fn the_daily_keep_is_not_sent_to_collect_from_an_empty_tank() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        answers(&node, VAULT, IVault::stateCall {}, VaultState::Open);
        answers(&node, VAULT, IVault::timersCall {}, timers());
        answers(
            &node,
            VAULT,
            IVault::tankCall {},
            a_bill_tank(0, 1_000_000, now - DAY, now - DAY),
        );
        answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
        answers(&node, VAULT, IVault::homeStationCall {}, STATION);
        answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// The same for a halted vault's bill after its grace.
    #[tokio::test]
    async fn a_halted_vaults_bill_is_not_collected_from_an_empty_tank() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_halted_vault(
            &node,
            now - 1_200,
            600,
            a_bill_tank(0, 1_000_000, now - 3_600, now - 3_600),
        );
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            &executor,
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        asks_back(&executor).await;

        assert_eq!(
            sent::<IVault::keepCall>(&node, VAULT),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A vault closes with dust left at its station, and anything that reaches
    /// the station afterwards stays there unless it is asked for: a closed
    /// vault's stopped station is asked to unwind, sending it home to the
    /// closed vault's cash for the holders still to redeem.
    #[tokio::test]
    async fn a_closed_vaults_station_is_asked_to_send_home_what_it_still_holds() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_closed_vault(
            &node,
            now - 1_200,
            true,
            a_bill_tank(0, 0, now - DAY, now - DAY),
        );
        answers(
            &node,
            STATION,
            IStation::valueCall {},
            IStation::valueReturn {
                mark: U256::from(5_000_000u64),
                exit: U256::from(5_000_000u64),
            },
        );
        answers(
            &node,
            STATION,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
            IStation::unwindReturn {
                sentHome: U256::from(5_000_000u64),
                blocked: false,
            },
        );
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        let unwinds = sent::<IStation::unwindCall>(&node, STATION)
            + sent::<IStation::unwindStoppedCall>(&node, STATION);
        assert!(
            unwinds >= 1,
            "five dollars left at the closed vault's station; sent: {:?}, quiet since {:?}",
            everything_sent(&node),
            memory.quiet_since
        );
        assert_eq!(
            memory.quiet_since, None,
            "a closed vault was left alone with its station still worth something"
        );
    }

    /// A vault closed inside its grace, with no key approved at its station, is
    /// not left alone for a day while its station still runs: the station is
    /// stopped as soon as the grace is over, so no key can be approved there
    /// after it.
    #[tokio::test]
    async fn a_vault_closed_inside_its_grace_has_its_station_stopped_when_the_grace_ends() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_closed_vault(
            &node,
            now - 60,
            false,
            a_bill_tank(0, 0, now - DAY, now - DAY),
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();
        assert_eq!(sent::<IStation::stop_0Call>(&node, STATION), 0);

        // The grace ends nine minutes later.
        node.lock().unwrap().timestamp = now + 600;
        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(
            sent::<IStation::stop_0Call>(&node, STATION),
            1,
            "the station is still running a minute after the grace; quiet since {:?}",
            memory.quiet_since
        );
    }

    /// A halted vault's bill is collected at the halt and once its grace is
    /// over, but a grace may outlast the tank escape's delay: an escape asked
    /// for inside it has the bill collected first, as an open vault's does.
    #[tokio::test]
    async fn an_escape_asked_for_inside_a_long_grace_has_its_bill_collected() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let halted_at = now - 2 * DAY;
        let mut escaping = a_bill_tank(50_000_000, 1_000_000, halted_at + 60, halted_at + 60);
        escaping.escapeRequestedAt = now - DAY + 60;
        a_halted_vault(&node, halted_at, 3 * DAY, escaping);
        reverts(&node, VAULT, IVault::closeCall {}, "not yet");
        let executor = executor(1_500_000, Arc::new(AtomicU64::new(now - 60))).await;
        let client = connect(&node).await;
        // The halt's bill was collected when the halt was first seen.
        let mut memory = Memory {
            asked_at_halt: true,
            bill_sent_as_of: halted_at + 60,
            daily_at: Some(now - 60),
            ..Memory::default()
        };

        for seconds in [0, 10] {
            node.lock().unwrap().timestamp = now + seconds;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
        }

        assert!(
            sent::<IVault::keepCall>(&node, VAULT) >= 1,
            "the escape opens in a minute with two days' usage uncollected: {:?}",
            everything_sent(&node)
        );
    }

    fn a_tank_holding(balance: u64) -> Tank {
        Tank {
            balance: U256::from(balance),
            keepAt: U256::ZERO,
            usageSettled: U256::ZERO,
            usageAsOf: 0,
            lastCollection: 0,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        }
    }

    /// An open vault whose station has `fresh` approved for the local
    /// driver, and nothing else to do.
    fn an_open_vault_with_a_key(node: &Shared, fresh: Address) {
        let local = name("local");
        answers(node, VAULT, IVault::stateCall {}, VaultState::Open);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::tankCall {}, a_tank_holding(50_000_000));
        answers(node, VAULT, IStationVault::haltedAtCall {}, 0);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![local]));
        answers(
            node,
            STATION,
            IStation::approvedKeyCall { driver: local },
            fresh,
        );
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    }

    fn a_station_log(signature: B256, key: Address, index: u64) -> serde_json::Value {
        crate::stand_in::a_log(
            STATION,
            signature,
            &[name("local"), B256::left_padding_from(key.as_slice())],
            5,
            index,
        )
    }

    /// How many times `key` was sent to be revoked. The stand-in takes a
    /// revoke of the key in place too, which the station would refuse while
    /// that key stands; that is the other step's business.
    fn revokes_of(node: &Shared, key: Address) -> usize {
        crate::stand_in::sent_calls::<IStation::revokeKeyCall>(node, STATION)
            .into_iter()
            .filter(|call| call.key == key)
            .count()
    }

    /// A handover from a key whose blessing lapsed replaces it at the
    /// station with a best-effort revoke; when that doesn't go through at
    /// the venue the station says `RevokeSkipped` and records the new key in
    /// its place, so the old one, still able to trade, is known only from
    /// the log.
    #[tokio::test]
    async fn a_key_left_approved_by_a_skipped_revoke_is_taken_back() {
        use alloy::sol_types::SolEvent;
        let node = node(BETWEEN_SLOTS);
        let old = Address::repeat_byte(0x0a);
        let fresh = Address::repeat_byte(0x0b);
        an_open_vault_with_a_key(&node, fresh);
        let skipped = a_station_log(IStation::RevokeSkipped::SIGNATURE_HASH, old, 0);
        let client = crate::stand_in::connect_with_logs_of(&node, STATION, vec![skipped]).await;
        // The station refuses while the old key still stands.
        let revoke_old = IStation::revokeKeyCall {
            driver: name("local"),
            key: old,
        };
        refuses(&node, STATION, revoke_old.clone());
        let mut memory = Memory::default();
        let round = async |memory: &mut Memory| {
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                memory,
            )
            .await
            .unwrap();
        };

        round(&mut memory).await;
        assert_eq!(revokes_of(&node, old), 0);
        assert_eq!(memory.skipped.len(), 1, "a refused revoke was forgotten");

        // Once its blessing has gone, the station takes it.
        takes(&node, STATION, revoke_old);
        round(&mut memory).await;
        assert_eq!(
            revokes_of(&node, old),
            1,
            "the key a skipped revoke left approved was never taken back: {:?}",
            everything_sent(&node)
        );
        assert!(memory.skipped.is_empty());

        // Done: not sent again.
        round(&mut memory).await;
        assert_eq!(revokes_of(&node, old), 1);
    }

    #[tokio::test]
    async fn a_skipped_revoke_finished_since_is_not_sent_again() {
        use alloy::sol_types::SolEvent;
        let node = node(BETWEEN_SLOTS);
        let old = Address::repeat_byte(0x0a);
        an_open_vault_with_a_key(&node, Address::repeat_byte(0x0b));
        let logs = vec![
            a_station_log(IStation::RevokeSkipped::SIGNATURE_HASH, old, 0),
            a_station_log(IStation::KeyRevoked::SIGNATURE_HASH, old, 1),
        ];
        let client = crate::stand_in::connect_with_logs_of(&node, STATION, logs).await;
        let mut memory = Memory::default();

        keep_one(
            &client,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await
        .unwrap();

        assert_eq!(revokes_of(&node, old), 0, "{:?}", everything_sent(&node));
    }

    /// A vault of the factory a replacement factory replaces, which never
    /// opened and is past its window: the simplest thing it is owed is a
    /// cancel.
    fn a_replaced_factorys_vault(node: &Shared, now: u64) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Activating);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::createdAtCall {}, now - 4 * DAY);
    }

    async fn one_tick(node: &Shared) {
        let client = connect(node).await;
        tick(
            shared::Chain::Base,
            &client,
            &deployments(),
            KEEPER,
            "http://127.0.0.1:1",
            &mut HashMap::new(),
            &mut Factories::default(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_vaults_of_a_replaced_factory_are_still_kept() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let replacement = deployments().factory;
        let replaced = Address::repeat_byte(0x77);
        let first = Address::repeat_byte(0x78);
        answers(&node, replacement, IVaultFactory::vaultsCall {}, vec![]);
        answers(
            &node,
            replacement,
            IReplacesFactory::previousCall {},
            replaced,
        );
        answers(&node, replaced, IVaultFactory::vaultsCall {}, vec![]);
        answers(&node, replaced, IReplacesFactory::previousCall {}, first);
        answers(&node, first, IVaultFactory::vaultsCall {}, vec![VAULT]);
        answers(
            &node,
            first,
            IReplacesFactory::previousCall {},
            Address::ZERO,
        );
        a_replaced_factorys_vault(&node, now);

        one_tick(&node).await;

        assert_eq!(
            sent::<IVault::cancelCall>(&node, VAULT),
            1,
            "a vault of a replaced factory was never looked at: {:?}",
            everything_sent(&node)
        );
    }

    /// The factory the deployment's replaces can't be read one round (a node
    /// that drops the read, not a revert): the walk goes on to the factory
    /// last read there, so its vaults are still kept, and the round says the
    /// list couldn't be read.
    #[tokio::test]
    async fn the_vaults_behind_a_factory_whose_previous_cannot_be_read_are_still_kept() {
        let node = node(BETWEEN_SLOTS);
        let replacement = deployments().factory;
        let replaced = Address::repeat_byte(0x77);
        answers(&node, replacement, IVaultFactory::vaultsCall {}, vec![]);
        answers(
            &node,
            replacement,
            IReplacesFactory::previousCall {},
            replaced,
        );
        answers(&node, replaced, IVaultFactory::vaultsCall {}, vec![VAULT]);
        answers(
            &node,
            replaced,
            IReplacesFactory::previousCall {},
            Address::ZERO,
        );
        let dropping = Arc::new(AtomicBool::new(false));
        let client = crate::stand_in::connect_failing(&node, {
            let dropping = dropping.clone();
            Arc::new(move |to, data: &[u8]| {
                dropping.load(Ordering::SeqCst)
                    && to == replacement
                    && data.starts_with(&IReplacesFactory::previousCall::SELECTOR)
            })
        })
        .await;
        let mut factories = Factories::default();

        let (vaults, unread) = every_vault(&client, replacement, &mut factories)
            .await
            .unwrap();
        assert_eq!((vaults, unread), (vec![VAULT], vec![]));

        dropping.store(true, Ordering::SeqCst);
        let (vaults, unread) = every_vault(&client, replacement, &mut factories)
            .await
            .unwrap();
        assert_eq!(
            (vaults, unread),
            (vec![VAULT], vec![replacement]),
            "the vaults behind a factory whose previous couldn't be read dropped out"
        );
    }

    #[tokio::test]
    async fn the_vaults_of_a_factory_that_names_no_earlier_one_are_kept() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        // A factory made before factories named the one they replace:
        // `previous` reverts.
        answers(
            &node,
            deployments().factory,
            IVaultFactory::vaultsCall {},
            vec![VAULT],
        );
        a_replaced_factorys_vault(&node, now);

        one_tick(&node).await;

        assert_eq!(sent::<IVault::cancelCall>(&node, VAULT), 1);
    }

    /// A claim a freeze period and a second old, last tried a few slots ago
    /// and told to wait the longest, as a claim that kept waiting all week
    /// is.
    fn an_overdue_claim(node: &Shared, now: u64, memory: &mut Memory) {
        a_waiting_claim(node, now, ClaimState::Waiting);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(
            node,
            VAULT,
            IVault::claimAtCall {
                claimId: U256::from(1u64),
            },
            Claim {
                owner: Address::repeat_byte(0x44),
                shares: U256::from(WAD),
                bookedAt: now - 7 * DAY - 1,
                lastAttemptAt: 0,
                state: ClaimState::Waiting,
                paid: U256::ZERO,
            },
        );
        let interval = timers().slotInterval;
        let slot = slot_of(now, interval);
        memory.claims_tried.insert(
            1,
            Tried {
                state: ClaimState::Waiting as u8,
                wait: MOST_SLOTS_BETWEEN_TRIES,
                next: slot + (MOST_SLOTS_BETWEEN_TRIES - 3) * interval,
                next_round: MOST_SLOTS_BETWEEN_TRIES,
            },
        );
    }

    #[tokio::test]
    async fn an_overdue_claim_is_tried_before_the_vault_is_frozen_for_it() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let mut memory = Memory::default();
        an_overdue_claim(&node, now, &mut memory);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let client = connect(&node).await;

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        // The try left it waiting, so the vault is frozen after it.
        let settle_claim = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::settleClaimCall::SELECTOR)
        );
        let freeze = format!(
            "{VAULT} {}",
            alloy::hex::encode_prefixed(IVault::freezeCall::SELECTOR)
        );
        assert_eq!(
            everything_sent(&node),
            [settle_claim, freeze],
            "the vault was frozen for a claim nobody tried this round"
        );
    }

    #[tokio::test]
    async fn an_overdue_claim_is_not_tried_outside_the_band_and_the_vault_is_still_frozen() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let mut memory = Memory::default();
        an_overdue_claim(&node, now, &mut memory);
        answers(&node, STATION, IStation::outsideBandCall {}, true);
        let client = connect(&node).await;

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(sent::<IVault::settleClaimCall>(&node, VAULT), 0);
        assert_eq!(sent::<IVault::freezeCall>(&node, VAULT), 1);
    }

    /// A closed vault whose stopped station a stranger sends `gift`
    /// dollar-units each slot, which a full unwind would bring home.
    fn a_closed_vault_with_a_gift(node: &Shared, start: u64, gift: u64) {
        answers(node, VAULT, IVault::stateCall {}, VaultState::Closed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(node, VAULT, IVault::tankCall {}, a_tank_holding(0));
        answers(node, VAULT, IVault::requestCountCall {}, U256::ZERO);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, VAULT, IStationVault::haltedAtCall {}, start - DAY);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, STATION, IStation::stoppedCall {}, true);
        answers(
            node,
            STATION,
            IStation::specCall {},
            a_spec(vec![name("local")]),
        );
        answers(
            node,
            STATION,
            IStation::approvedKeyCall {
                driver: name("local"),
            },
            Address::ZERO,
        );
        a_stopped_station_at(node, gift, gift, gift, false);
    }

    /// The slots, of sixteen, the closed vault's station was asked to unwind
    /// in.
    async fn closed_unwinds(gift: u64) -> Vec<u64> {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        a_closed_vault_with_a_gift(&node, start, gift);
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let interval = timers().slotInterval;
        let mut unwound_in = Vec::new();
        for slot in 0..16u64 {
            node.lock().unwrap().timestamp = start + slot * interval;
            let before = sent::<IStation::unwindCall>(&node, STATION);
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                "http://127.0.0.1:1",
                VAULT,
                &mut memory,
            )
            .await
            .unwrap();
            if sent::<IStation::unwindCall>(&node, STATION) > before {
                unwound_in.push(slot);
            }
        }
        unwound_in
    }

    #[tokio::test]
    async fn a_closed_station_is_not_unwound_for_a_gift_of_dust() {
        // One driver: dust is four dollar-units.
        assert_eq!(closed_unwinds(1).await, [] as [u64; 0]);
    }

    /// The vault's dust is a few millionths of a dollar: a stranger who
    /// sends the station five millionths each slot doesn't have the keeper
    /// pay a full unwind (every driver's close, then a transfer home) to
    /// bring it home. Less than a cent is not worth an unwind.
    #[tokio::test]
    async fn a_closed_station_is_not_unwound_for_less_than_a_cent() {
        assert_eq!(closed_unwinds(5).await, [] as [u64; 0]);
        assert_eq!(
            closed_unwinds(UNWIND_WORTH_AT_LEAST - 1).await,
            [] as [u64; 0]
        );
    }

    #[tokio::test]
    async fn a_closed_station_is_unwound_every_slot_while_it_brings_home_a_cent() {
        assert_eq!(
            closed_unwinds(UNWIND_WORTH_AT_LEAST).await,
            (0..16).collect::<Vec<_>>()
        );
    }

    fn overdue_claim(now: u64, state: ClaimState) -> Claim {
        Claim {
            owner: Address::repeat_byte(0x44),
            shares: U256::from(WAD),
            // A freeze period and a second ago.
            bookedAt: now - 7 * DAY - 1,
            lastAttemptAt: 0,
            state,
            paid: U256::ZERO,
        }
    }

    fn one_claim(node: &Shared, claim: Claim) {
        answers(node, VAULT, IVault::claimCountCall {}, U256::from(1u64));
        answers(
            node,
            VAULT,
            IVault::claimAtCall {
                claimId: U256::from(1u64),
            },
            claim,
        );
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::outsideBandCall {}, false);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
    }

    /// The vault's one valuation: recorded at `time`, priced or void.
    fn one_valuation(node: &Shared, time: u64, priced: bool) {
        answers(node, VAULT, IVault::valuationCountCall {}, U256::from(1u64));
        answers(
            node,
            VAULT,
            IVault::valuationAtCall { index: U256::ZERO },
            Valuation {
                time,
                status: if priced {
                    ValuationStatus::Priced
                } else {
                    ValuationStatus::Void
                },
                gross: U256::from(1u64),
                grossExit: U256::from(1u64),
                supply: U256::from(1u64),
                fee: U256::ZERO,
                feeExit: U256::ZERO,
                cash: U256::ZERO,
                postsMissing: 0,
            },
        );
    }

    /// A claim a week old is tried before the vault is frozen for it. When
    /// the try finds the cash covers it, the vault queues it, but a queued
    /// claim is still unpaid: the contract lets anyone freeze for it a week
    /// after booking, whatever was tried, and so does the keeper.
    #[tokio::test]
    async fn an_overdue_claim_its_own_try_queues_still_freezes_the_vault() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        one_claim(&node, overdue_claim(now, ClaimState::Waiting));
        // The try queues it: the cash covers it now.
        let hook: Hook = Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::settleClaimCall::SELECTOR {
                answers(
                    node,
                    VAULT,
                    IVault::claimAtCall {
                        claimId: U256::from(1u64),
                    },
                    Claim {
                        lastAttemptAt: now,
                        ..overdue_claim(now, ClaimState::Queued)
                    },
                );
            }
        });
        let client = connect_with_hook(&node, hook).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(sent::<IVault::settleClaimCall>(&node, VAULT), 1);
        assert_eq!(
            sent::<IVault::freezeCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// A claim queued in the last slot of its week, before any valuation
    /// could pay it, may still be frozen for: the contract counts the week
    /// from booking and allows it, and the keeper follows the same rule
    /// rather than guess when a valuation will come.
    #[tokio::test]
    async fn a_claim_queued_in_its_last_slot_may_still_be_frozen_for() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        one_claim(
            &node,
            Claim {
                // Queued by a try a few seconds ago.
                lastAttemptAt: now - 5,
                ..overdue_claim(now, ClaimState::Queued)
            },
        );
        one_valuation(&node, now - 100, true);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        // A queued claim has nothing to try.
        assert_eq!(sent::<IVault::settleClaimCall>(&node, VAULT), 0);
        assert_eq!(sent::<IVault::freezeCall>(&node, VAULT), 1);
    }

    /// A claim the cash covered when it was last tried is queued for the next
    /// valuation. While the market stays outside the band no valuation is
    /// recorded and nothing is priced, so it is never paid: a week after
    /// booking it freezes the vault all the same, as a pending or waiting one
    /// in the same market does.
    #[tokio::test]
    async fn a_queued_claim_the_market_keeps_from_being_priced_still_freezes_the_vault() {
        let now = SLOT_START;
        let booked = now - 30 * DAY;
        let queued_at = booked + 6 * DAY;
        let node = node(now);
        one_claim(
            &node,
            Claim {
                bookedAt: booked,
                lastAttemptAt: queued_at,
                ..overdue_claim(now, ClaimState::Queued)
            },
        );
        // The last priced valuation is the one the try that queued it read.
        one_valuation(&node, queued_at - 100, true);
        answers(&node, STATION, IStation::outsideBandCall {}, true);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        // The slot's valuation is not recorded while the market is outside.
        price(
            &client,
            KEEPER,
            VAULT,
            &timers(),
            1,
            true,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 0);

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            sent::<IVault::freezeCall>(&node, VAULT),
            1,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// While a priced valuation waits to be settled, its settle may pay the
    /// claim, so the vault isn't frozen for it that round; nor when whether
    /// one is waiting can't be read. The freeze is tried again next round.
    #[tokio::test]
    async fn a_queued_claim_is_not_frozen_for_when_whether_it_is_settled_cannot_be_read() {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let queued = Claim {
            lastAttemptAt: now - 3 * interval,
            ..overdue_claim(now, ClaimState::Queued)
        };
        for (unsettled, freezes) in [(Some(true), 0), (None, 0), (Some(false), 1)] {
            let node = node(now);
            one_claim(&node, queued.clone());
            one_valuation(&node, slot_of(now, interval), true);
            match unsettled {
                Some(unsettled) => {
                    answers(&node, VAULT, IStationVault::unsettledCall {}, unsettled)
                }
                None => reverts(
                    &node,
                    VAULT,
                    IStationVault::unsettledCall {},
                    "the node dropped this read",
                ),
            }
            let client = connect(&node).await;
            let mut memory = Memory::default();

            claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
                .await
                .unwrap();

            assert_eq!(
                sent::<IVault::freezeCall>(&node, VAULT),
                freezes,
                "unsettled {unsettled:?}: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// A claim's try is refused while a priced valuation is still settling,
    /// so an overdue one reads as unpaid after it: the vault isn't frozen for
    /// it that round, since the cash may cover it once the settle is done.
    #[tokio::test]
    async fn an_overdue_claim_that_could_not_be_tried_while_a_valuation_settles_does_not_freeze_the_vault()
     {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        one_claim(&node, overdue_claim(now, ClaimState::Waiting));
        answers(&node, VAULT, IStationVault::unsettledCall {}, true);
        refuses(
            &node,
            VAULT,
            IVault::settleClaimCall {
                claimId: U256::from(1u64),
            },
        );
        let client = connect(&node).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            sent::<IVault::freezeCall>(&node, VAULT),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    /// Units left unserved as long as a claim may wait freeze the vault, but
    /// not while a priced valuation is settling (the serve is refused until
    /// it is done, and may pay them then), nor when that can't be read.
    #[tokio::test]
    async fn unserved_units_do_not_freeze_the_vault_while_a_valuation_may_be_settling() {
        let now = BETWEEN_SLOTS;
        let holder = Address::repeat_byte(0x02);
        for (unsettled, freezes) in [(Some(true), 0), (None, 0), (Some(false), 1)] {
            let node = node(now);
            answers(
                &node,
                VAULT,
                IVault::unitsOfCall { holder },
                IVault::unitsOfReturn {
                    units: U256::from(5u64),
                    claimable: U256::ZERO,
                },
            );
            answers(
                &node,
                VAULT,
                IVault::unitsSinceCall { holder },
                now - 7 * DAY,
            );
            refuses(&node, VAULT, IVault::serveProRataCall { holder });
            match unsettled {
                Some(unsettled) => {
                    answers(&node, VAULT, IStationVault::unsettledCall {}, unsettled)
                }
                None => reverts(
                    &node,
                    VAULT,
                    IStationVault::unsettledCall {},
                    "the node dropped this read",
                ),
            }
            let client = connect(&node).await;
            let mut memory = Memory {
                holders: BTreeSet::from([holder]),
                ..Memory::default()
            };

            units(&client, KEEPER, VAULT, 1, now, 7 * DAY, 10, 0, &mut memory)
                .await
                .unwrap();

            assert_eq!(
                sent::<IVault::freezeUnitsCall>(&node, VAULT),
                freezes,
                "unsettled {unsettled:?}: {:?}",
                everything_sent(&node)
            );
        }
    }

    /// One claim that can't be read this round is left for the next; the
    /// claims after it are still tried.
    #[tokio::test]
    async fn a_claim_that_cannot_be_read_holds_up_no_other() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        a_waiting_claim(&node, now, ClaimState::Pending);
        answers(&node, VAULT, IVault::claimCountCall {}, U256::from(2u64));
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let first = IVault::claimAtCall {
            claimId: U256::from(1u64),
        }
        .abi_encode();
        let failing: Failing = Arc::new(move |to, data| to == VAULT && data == first.as_slice());
        let client = connect_failing(&node, failing).await;
        let mut memory = Memory::default();

        claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
            .await
            .unwrap();

        assert_eq!(
            crate::stand_in::sent_calls::<IVault::settleClaimCall>(&node, VAULT)
                .iter()
                .map(|call| call.claimId)
                .collect::<Vec<_>>(),
            [U256::from(2u64)]
        );
        assert_eq!(
            memory.claims_from, 0,
            "an unread claim was passed over for good"
        );
    }

    /// One holder whose units can't be read this round is left for the next;
    /// the holders after it are still served.
    #[tokio::test]
    async fn a_holder_whose_units_cannot_be_read_holds_up_no_other() {
        let now = BETWEEN_SLOTS;
        let node = node(now);
        let unread = Address::repeat_byte(0x01);
        let holder = Address::repeat_byte(0x02);
        answers(
            &node,
            VAULT,
            IVault::unitsOfCall { holder },
            IVault::unitsOfReturn {
                units: U256::from(5u64),
                claimable: U256::ZERO,
            },
        );
        answers(&node, VAULT, IVault::unitsSinceCall { holder }, now);
        let failing: Failing = Arc::new(move |to, data| {
            to == VAULT
                && IVault::unitsOfCall::abi_decode(data).is_ok_and(|call| call.holder == unread)
        });
        let client = connect_failing(&node, failing).await;
        let mut memory = Memory {
            holders: BTreeSet::from([unread, holder]),
            ..Memory::default()
        };

        let all_served = units(&client, KEEPER, VAULT, 1, now, 7 * DAY, 10, 0, &mut memory)
            .await
            .unwrap();

        assert!(!all_served);
        assert_eq!(
            crate::stand_in::sent_calls::<IVault::serveProRataCall>(&node, VAULT)
                .iter()
                .map(|call| call.holder)
                .collect::<Vec<_>>(),
            [holder]
        );
        assert!(memory.holders.contains(&unread));
    }

    /// A holder's serve that passes its estimate and is mined and reverts,
    /// every time, paid its gas and served nothing: it is not sent every
    /// round, but next slot, then two on, four and so on, and no sooner than
    /// as many of the keeper's rounds on, so a clock that runs fast can't make
    /// every round pay for one. Once one goes through, the next that reverts
    /// waits a slot again.
    #[tokio::test]
    async fn a_serve_mined_and_reverted_every_time_is_sent_less_and_less_often() {
        let now = BETWEEN_SLOTS;
        let interval = timers().slotInterval;
        let node = node(now);
        let holder = Address::repeat_byte(0x02);
        answers(
            &node,
            VAULT,
            IVault::unitsOfCall { holder },
            IVault::unitsOfReturn {
                units: U256::from(5u64),
                claimable: U256::ZERO,
            },
        );
        answers(&node, VAULT, IVault::unitsSinceCall { holder }, now);
        let reverting = Arc::new(AtomicBool::new(true));
        let client = crate::stand_in::connect_reverting(&node, reverting.clone()).await;
        let serves = || sent::<IVault::serveProRataCall>(&node, VAULT);
        let rounds_served = async |memory: &mut Memory, from: u64, step: u64, rounds: u64| {
            let mut served_in = Vec::new();
            for round in 0..rounds {
                let before = serves();
                memory.rounds += 1;
                units(
                    &client,
                    KEEPER,
                    VAULT,
                    1,
                    from + round * step,
                    7 * DAY,
                    interval,
                    0,
                    memory,
                )
                .await
                .unwrap();
                if serves() > before {
                    served_in.push(round);
                }
            }
            served_in
        };

        // A round a slot.
        let mut memory = Memory {
            holders: BTreeSet::from([holder]),
            ..Memory::default()
        };
        assert_eq!(
            rounds_served(&mut memory, now, interval, 40).await,
            [0, 1, 3, 7, 15, 31]
        );

        // An hour a round: the rounds are counted too.
        let mut memory = Memory {
            holders: BTreeSet::from([holder]),
            ..Memory::default()
        };
        assert_eq!(
            rounds_served(&mut memory, now, 3_600, 20).await,
            [0, 1, 3, 7, 15]
        );

        // One that goes through ends the wait.
        reverting.store(false, Ordering::SeqCst);
        let mut at = now + DAY;
        while memory.serve_again.contains_key(&holder) {
            rounds_served(&mut memory, at, 0, 1).await;
            at += 3_600;
        }
        reverting.store(true, Ordering::SeqCst);
        rounds_served(&mut memory, at, 0, 1).await;
        assert_eq!(
            memory.serve_again.get(&holder).map(|again| again.wait),
            Some(1)
        );
    }

    /// A killed vault past its grace, its station stopped and empty, nothing
    /// owed, with a bill still to collect for the grace.
    fn a_halted_empty_vault(node: &Shared, now: u64) {
        let halted_at = now - 1_000;
        answers(node, VAULT, IVault::stateCall {}, VaultState::Killed);
        answers(node, VAULT, IVault::timersCall {}, timers());
        answers(
            node,
            VAULT,
            IVault::tankCall {},
            a_bill_tank(50_000_000, 1_000_000, halted_at, halted_at),
        );
        answers(node, VAULT, IStationVault::haltedAtCall {}, halted_at);
        answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(node, STATION, IStation::outsideBandCall {}, false);
        a_stopped_station_at(node, 0, 0, 0, false);
        answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    }

    /// An executor that serves no total, after `seconds` of chain time: one
    /// that has stopped answering is waited on while the chain moves on.
    async fn a_slow_executor(node: &Shared, seconds: u64) -> String {
        let app =
            axum::Router::new()
                .route(
                    "/usage/{chain}/{vault}",
                    axum::routing::get(
                        |axum::extract::State((node, seconds)): axum::extract::State<(
                            Shared,
                            u64,
                        )>| async move {
                            node.lock().unwrap().timestamp += seconds;
                            axum::http::StatusCode::SERVICE_UNAVAILABLE
                        },
                    ),
                )
                .with_state((node.clone(), seconds));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        url
    }

    /// How many times, in twenty rounds started at every second of ten-second
    /// slots, the halted vault was valued or sent to the Closer.
    async fn valuations_or_closes(executor_seconds: u64) -> usize {
        let closer = Address::repeat_byte(0x66);
        let with_closer = Deployments {
            closer,
            ..deployments()
        };
        let start = SLOT_START;
        let node = node(start);
        closes(&node, closer);
        a_halted_empty_vault(&node, start);
        let executor = a_slow_executor(&node, executor_seconds).await;
        let client = connect(&node).await;
        let mut memory = Memory::default();
        for round in 0..20u64 {
            // Each round starts one slot and one second further on.
            node.lock().unwrap().timestamp = start + round * 10 + round % 10;
            keep_one(&client, KEEPER, &with_closer, &executor, VAULT, &mut memory)
                .await
                .unwrap();
            asks_back(&executor).await;
        }
        sent::<IVault::recordValuationCall>(&node, VAULT)
            + sent::<ICloser::closeHaltedCall>(&node, closer)
    }

    /// The executor is never asked between the judgement that a send will
    /// land inside a slot's window and that send: one that takes longer to
    /// answer than the window lasts would otherwise push every such send past
    /// it, and a halted vault would never be valued or closed.
    #[tokio::test]
    async fn a_slow_executor_does_not_keep_a_halted_vault_from_ever_being_valued() {
        assert!(valuations_or_closes(0).await >= 1);
        assert!(
            valuations_or_closes(5).await >= 1,
            "an executor five seconds slow kept a halted, empty vault from being valued or closed in twenty slots"
        );
    }

    /// The devnet moves its clock an hour a block, so every round of the
    /// keeper finds hundreds of slots gone: a claim that keeps waiting is
    /// still tried ever less often, counted in rounds.
    #[tokio::test]
    async fn a_claim_that_keeps_waiting_on_a_fast_clock_is_tried_ever_less_often() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        a_waiting_claim(&node, start, ClaimState::Waiting);
        answers(&node, STATION, IStation::outsideBandCall {}, false);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        let mut tried_in = Vec::new();
        for round in 0..16u64 {
            let before = sent::<IVault::settleClaimCall>(&node, VAULT);
            node.lock().unwrap().timestamp = start + round * 3_600;
            claims(&client, KEEPER, VAULT, &timers(), 0, &mut memory)
                .await
                .unwrap();
            if sent::<IVault::settleClaimCall>(&node, VAULT) > before {
                tried_in.push(round);
            }
        }
        assert_eq!(tried_in, [0, 1, 3, 7, 15]);
    }

    /// The devnet moves its clock an hour a block: a `keep` carrying the
    /// bill that is mined and reverts every time is still sent ever less
    /// often, counted in rounds, not at every round.
    #[tokio::test]
    async fn a_keep_reverted_every_time_on_a_fast_clock_is_sent_ever_less_often() {
        let start = BETWEEN_SLOTS;
        let node = node(start);
        // A vault of its own: the refused bills are counted by vault.
        let vault = Address::repeat_byte(0x5a);
        answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(
            &node,
            vault,
            IVault::tankCall {},
            a_tank(start, 1_000_000, start - 3_600),
        );
        answers(&node, vault, IStationVault::haltedAtCall {}, 0);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
        let executor = crate::stand_in::moving_executor(
            Arc::new(AtomicU64::new(1_500_000)),
            Arc::new(AtomicU64::new(start - 600)),
        )
        .await;
        let client =
            crate::stand_in::connect_reverting(&node, Arc::new(AtomicBool::new(true))).await;
        let mut memory = Memory::default();
        let billed = || {
            crate::stand_in::sent_calls::<IVault::keepCall>(&node, vault)
                .iter()
                .filter(|keep| !keep.total.is_zero())
                .count()
        };

        // The daily ask goes out in the first round; its answer is collected
        // from the next.
        let mut sent_in = Vec::new();
        for round in 0..16u64 {
            let before = billed();
            node.lock().unwrap().timestamp = start + round * 3_600;
            keep_one(
                &client,
                KEEPER,
                &deployments(),
                &executor,
                vault,
                &mut memory,
            )
            .await
            .unwrap();
            asks_back(&executor).await;
            if billed() > before {
                sent_in.push(round);
            }
        }
        assert_eq!(sent_in, [1, 2, 4, 8], "{:?}", everything_sent(&node));
    }

    /// The devnet moves its clock an hour a block: a simulation's wait is
    /// over at every round, and an unwind that would do nothing is still
    /// never paid for.
    #[tokio::test]
    async fn an_unwind_that_would_do_nothing_on_a_fast_clock_is_never_paid_for() {
        let node = node(BETWEEN_SLOTS);
        answers(&node, VAULT, IStationVault::unsettledCall {}, false);
        a_stopped_station_at(&node, 6_000_000_000, 0, 0, false);
        leaves_after_unwind(&node, STATION, 6_000_000_000);
        let mut memory = Memory::default();

        for round in 0..16u64 {
            node.lock().unwrap().timestamp = BETWEEN_SLOTS + round * 3_600;
            unwind_round(&node, &mut memory).await;
        }
        assert_eq!(sent::<IStation::unwindCall>(&node, STATION), 0);
    }

    /// One request of a closed vault that can't be read this round is left
    /// for the next; the requests after it are still paid out.
    #[tokio::test]
    async fn a_request_that_cannot_be_read_holds_up_no_other() {
        let node = node(BETWEEN_SLOTS);
        answers(&node, VAULT, IVault::requestCountCall {}, U256::from(2u64));
        answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
        answers(
            &node,
            VAULT,
            IVault::requestAtCall {
                requestId: U256::from(2u64),
            },
            chain::bindings::vault::Request {
                owner: Address::repeat_byte(0x44),
                kind: RequestKind::Redeem,
                earliest: BETWEEN_SLOTS,
                done: false,
                amount: U256::from(WAD),
                r#ref: U256::ZERO,
            },
        );
        let first = IVault::requestAtCall {
            requestId: U256::from(1u64),
        }
        .abi_encode();
        let failing: Failing = Arc::new(move |to, data| to == VAULT && data == first.as_slice());
        let client = connect_failing(&node, failing).await;
        let mut memory = Memory::default();

        let done = paid_out(&client, KEEPER, VAULT, &mut memory).await.unwrap();

        assert!(!done);
        assert_eq!(
            crate::stand_in::sent_calls::<IVault::settleClosedCall>(&node, VAULT)
                .iter()
                .map(|call| call.requestId)
                .collect::<Vec<_>>(),
            [U256::from(2u64)]
        );
        assert_eq!(
            memory.requests_from, 0,
            "an unread request was passed over for good"
        );
    }

    // Valuing only when a valuation is wanted ----------------------------------

    /// A vault priced at `valued_at`, its payday a week off, with no request
    /// waiting for its price and no staged close behind it: nothing waits
    /// for a valuation.
    fn a_quiet_vault(node: &Shared, valued_at: u64) {
        answers(node, VAULT, IVault::valuationCountCall {}, U256::from(1u64));
        answers(
            node,
            VAULT,
            IVault::valuationAtCall { index: U256::ZERO },
            Valuation {
                time: valued_at,
                status: ValuationStatus::Priced,
                gross: U256::from(1_000_000_001u64),
                grossExit: U256::from(1_000_000_001u64),
                supply: U256::from(WAD),
                fee: U256::ZERO,
                feeExit: U256::ZERO,
                cash: U256::from(1_000_000_000u64),
                postsMissing: 0,
            },
        );
        answers(
            node,
            VAULT,
            IVault::lastPricedCall {},
            IVault::lastPricedReturn {
                priced: true,
                index: U256::ZERO,
                time: valued_at,
            },
        );
        answers(node, VAULT, IVault::lastPaymentCall {}, SLOT_START - DAY);
        answers(node, VAULT, IVault::nextUnpricedCall {}, 0);
        answers(node, VAULT, IVault::needsValuationFirstCall {}, false);
        answers(
            node,
            VAULT,
            IVault::closeWantsValuationCall { time: 0 },
            false,
        );
        answers(node, VAULT, IStationVault::unsettledCall {}, false);
        answers(node, VAULT, IVault::homeStationCall {}, STATION);
        answers(node, STATION, IStation::outsideBandCall {}, false);
    }

    /// One round's valuation step at the head's time `now`, on a chain of
    /// one-second blocks.
    async fn price_at(node: &Shared, client: &Client, now: u64, halt: Halt, memory: &mut Memory) {
        node.lock().unwrap().timestamp = now;
        price(client, KEEPER, VAULT, &timers(), 1, true, halt, memory).await;
    }

    #[tokio::test]
    async fn a_vault_with_nothing_waiting_is_not_valued() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - 10);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        for slot in 0..30 {
            price_at(
                &node,
                &client,
                SLOT_START + 10 * slot,
                Halt::Running,
                &mut memory,
            )
            .await;
        }

        assert_eq!(
            sent::<IVault::recordValuationCall>(&node, VAULT),
            0,
            "{:?}",
            everything_sent(&node)
        );
    }

    #[tokio::test]
    async fn a_waiting_deposit_is_valued_at_its_slot() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - 10);
        // Asked for after the last valuation, to be priced at the next slot.
        answers(&node, VAULT, IVault::nextUnpricedCall {}, SLOT_START + 10);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        // A valuation in this slot wouldn't price it.
        price_at(&node, &client, SLOT_START, Halt::Running, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 0);

        price_at(&node, &client, SLOT_START + 10, Halt::Running, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    #[tokio::test]
    async fn payday_gets_its_valuation_with_nothing_queued() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - 10);
        // Paid more than a week ago: the Monday after has passed.
        answers(
            &node,
            VAULT,
            IVault::lastPaymentCall {},
            SLOT_START - 8 * DAY,
        );
        let client = connect(&node).await;

        price_at(
            &node,
            &client,
            SLOT_START,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    #[tokio::test]
    async fn a_vault_nobody_moves_in_is_valued_once_a_day() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY + 10);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        price_at(&node, &client, SLOT_START, Halt::Running, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 0);

        price_at(&node, &client, SLOT_START + 10, Halt::Running, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    /// A vault never priced is valued: the heartbeat counts from nothing.
    #[tokio::test]
    async fn a_vault_never_priced_is_valued() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - 10);
        // Its one valuation was void.
        answers(
            &node,
            VAULT,
            IVault::lastPricedCall {},
            IVault::lastPricedReturn {
                priced: false,
                index: U256::ZERO,
                time: 0,
            },
        );
        let client = connect(&node).await;

        price_at(
            &node,
            &client,
            SLOT_START,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    /// What can't be read is taken as wanting a valuation: one too many costs
    /// a transaction, one too few keeps a depositor waiting.
    #[tokio::test]
    async fn a_vault_whose_requests_cannot_be_read_is_valued() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - 10);
        reverts(&node, VAULT, IVault::nextUnpricedCall {}, "");
        let client = connect(&node).await;

        price_at(
            &node,
            &client,
            SLOT_START,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    /// Once a close staged at the station has ended, the vault serves no
    /// claim and no units until a valuation is priced, and says so: one is
    /// sent while a claim or a unit holder waits, and none while nobody does
    /// or once the vault no longer needs one. Who waits is read in the round
    /// that decides, with nothing remembered from an earlier one: a claim
    /// booked since the last look at the claims is seen, and one that can't
    /// be read is taken as waiting.
    #[tokio::test]
    async fn a_staged_close_that_ended_is_followed_by_a_valuation_while_a_claim_or_units_wait() {
        // Whether a claim waits: none, one, or one that can't be read.
        #[derive(Clone, Copy, Debug)]
        enum Claims {
            None,
            Waiting,
            Unread,
        }
        for (needs, claims, units, valued) in [
            (true, Claims::None, false, 0),
            (true, Claims::Waiting, false, 1),
            (true, Claims::Unread, false, 1),
            (true, Claims::None, true, 1),
            (false, Claims::Waiting, true, 0),
        ] {
            let node = node(SLOT_START);
            a_quiet_vault(&node, SLOT_START - 10);
            answers(&node, VAULT, IVault::needsValuationFirstCall {}, needs);
            answers(
                &node,
                VAULT,
                IVault::unitsCall {},
                IVault::unitsReturn {
                    total: U256::from(u64::from(units) * WAD),
                    reserve: U256::ZERO,
                },
            );
            match claims {
                Claims::None => {
                    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
                }
                Claims::Waiting => {
                    one_claim(&node, overdue_claim(SLOT_START, ClaimState::Pending));
                }
                Claims::Unread => {
                    answers(&node, VAULT, IVault::claimCountCall {}, U256::from(1u64));
                    reverts(
                        &node,
                        VAULT,
                        IVault::claimAtCall {
                            claimId: U256::from(1u64),
                        },
                        "",
                    );
                }
            }
            let client = connect(&node).await;

            price_at(
                &node,
                &client,
                SLOT_START,
                Halt::Running,
                &mut Memory::default(),
            )
            .await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                valued,
                "needed first: {needs}, claims: {claims:?}, units waiting: {units}"
            );
        }
    }

    /// A holder's serve that brings nothing home clears the units only as
    /// dust, judged on what the home station holds now or what the last
    /// priced valuation counted there, whichever is more: once the station
    /// has emptied since, the vault refuses the serve until a newer valuation
    /// counts it. The keeper wants one then, rather than leaving the units to
    /// wait for the day's; not while the serve goes through, is refused for
    /// another reason, or waits on a settlement, nor when the station has
    /// only moved with the market.
    #[tokio::test]
    async fn waiting_units_get_a_valuation_when_their_serve_needs_one() {
        let holder = Address::repeat_byte(0x55);
        // What the serve meets, what the station holds now, whether a
        // settlement is owed, and whether a valuation is sent.
        for (serve, mark, owed, valued) in [
            (Some("NotYet()"), 0u64, false, 1),
            (Some("NotYet()"), 400_000_000, false, 0),
            (Some("NotYet()"), 0, true, 0),
            (Some("NotAllowed()"), 0, false, 0),
            (None, 0, false, 0),
        ] {
            let node = node(SLOT_START);
            a_quiet_vault(&node, SLOT_START - 10);
            // The last valuation counted 500 dollars at the stations.
            answers(
                &node,
                VAULT,
                IVault::valuationAtCall { index: U256::ZERO },
                Valuation {
                    time: SLOT_START - 10,
                    status: ValuationStatus::Priced,
                    gross: U256::from(1_500_000_001u64),
                    grossExit: U256::from(1_500_000_001u64),
                    supply: U256::from(WAD),
                    fee: U256::ZERO,
                    feeExit: U256::ZERO,
                    cash: U256::from(1_000_000_000u64),
                    postsMissing: 0,
                },
            );
            answers(
                &node,
                STATION,
                IStation::valueCall {},
                IStation::valueReturn {
                    mark: U256::from(mark),
                    exit: U256::from(mark),
                },
            );
            answers(&node, VAULT, IStationVault::unsettledCall {}, owed);
            match serve {
                Some(reason) => reverts(&node, VAULT, IVault::serveProRataCall { holder }, reason),
                None => answers(
                    &node,
                    VAULT,
                    IVault::serveProRataCall { holder },
                    IVault::serveProRataReturn {},
                ),
            }
            let client = connect(&node).await;
            let mut memory = Memory {
                holders: BTreeSet::from([holder]),
                ..Memory::default()
            };

            price_at(&node, &client, SLOT_START, Halt::Running, &mut memory).await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                valued,
                "serve: {serve:?}, station: {mark}, owed: {owed}"
            );
        }
    }

    /// A halted vault is valued for its close when the vault says a
    /// valuation in the slot would let it close, and not otherwise.
    #[tokio::test]
    async fn a_halted_vault_is_valued_for_its_close_when_the_vault_says_one_would_let_it() {
        for (wants, valued) in [(false, 0), (true, 1)] {
            let node = node(SLOT_START);
            a_quiet_vault(&node, SLOT_START - 10);
            answers(
                &node,
                VAULT,
                IVault::closeWantsValuationCall { time: 0 },
                wants,
            );
            let client = connect(&node).await;

            price_at(
                &node,
                &client,
                SLOT_START,
                Halt::Halted,
                &mut Memory::default(),
            )
            .await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                valued,
                "the close wants one: {wants}"
            );
        }
    }

    /// A close staged at the station voids any valuation recorded meanwhile:
    /// none is paid for.
    #[tokio::test]
    async fn a_valuation_is_not_sent_while_a_close_is_staged() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY);
        answers(
            &node,
            STATION,
            IStation::stageCall {},
            chain::bindings::station::Stage {
                fraction: U256::from(WAD / 4),
                owed: U256::ZERO,
                estimate: U256::ZERO,
                fetched: U256::ZERO,
                cost: U256::ZERO,
                part: U256::ZERO,
                arrivedAt: U256::ZERO,
                stagedAt: SLOT_START - 5,
                stagedBlock: 15,
                sweptAt: 0,
                sweptBlock: 0,
                window: 10,
                delay: 0,
                forVault: true,
            },
        );
        let client = connect(&node).await;

        price_at(
            &node,
            &client,
            SLOT_START,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 0);
    }

    /// On a chain of two-second blocks a valuation sent with the head one
    /// second into the slot lands at the window's last second if it makes
    /// the next block, and outside it if it misses that block: the vault
    /// would revert it, and it would still be paid for. Simulated landing a
    /// block late, it isn't sent; the next slot's, sent a second before the
    /// slot begins, lands inside the window even a block late, and is.
    #[tokio::test]
    async fn a_valuation_that_would_revert_is_not_sent() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY);
        keeps_the_window(&node, timers().slotInterval, timers().slotWindow);
        let client = connect(&node).await;
        let mut memory = Memory::default();
        let at = async |now: u64, memory: &mut Memory| {
            node.lock().unwrap().timestamp = now;
            price(
                &client,
                KEEPER,
                VAULT,
                &timers(),
                2,
                true,
                Halt::Running,
                memory,
            )
            .await;
        };

        at(SLOT_START + 1, &mut memory).await;
        assert_eq!(
            sent::<IVault::recordValuationCall>(&node, VAULT),
            0,
            "simulated at {:?}",
            simulated_at::<IVault::recordValuationCall>(&node, VAULT)
        );
        // Only late: the vault is not said to refuse it.
        assert_eq!(memory.valuations_refused.slots, 0);

        at(SLOT_START + 9, &mut memory).await;
        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
        assert_eq!(
            simulated_at::<IVault::recordValuationCall>(&node, VAULT).last(),
            Some(&(SLOT_START + 13))
        );
    }

    /// A valuation the vault would refuse wherever it lands is not sent, and
    /// counts toward the vault refusing its valuations.
    #[tokio::test]
    async fn a_valuation_the_vault_refuses_is_not_sent_and_is_counted_refused() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY);
        refuses(&node, VAULT, IVault::recordValuationCall {});
        let client = connect(&node).await;
        let mut memory = Memory::default();

        price_at(&node, &client, SLOT_START, Halt::Running, &mut memory).await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 0);
        assert_eq!(memory.valuations_refused.slots, 1);
        assert_eq!(
            simulated_at::<IVault::recordValuationCall>(&node, VAULT),
            [SLOT_START + 2, SLOT_START + 1]
        );
    }

    /// A node that runs a call at the head's time, whatever time it is
    /// given, would judge a valuation by the moment it was sent rather than
    /// the one it lands at: sent a second before a slot, it would seem to
    /// land outside the window either way and never be sent. Such a node is
    /// found out and not trusted to simulate; the valuation goes on its gas
    /// estimate, as before simulations, while a node that runs the call at
    /// the time given judges it as it lands.
    #[tokio::test]
    async fn a_node_that_ignores_the_block_time_is_not_trusted_to_simulate() {
        for ignores in [false, true] {
            let node = node(SLOT_START - 1);
            a_quiet_vault(&node, SLOT_START - DAY);
            keeps_the_window(&node, timers().slotInterval, timers().slotWindow);
            if ignores {
                crate::stand_in::ignores_block_time(&node);
            }
            let client = connect(&node).await;
            let mut memory = Memory::default();

            price(
                &client,
                KEEPER,
                VAULT,
                &timers(),
                2,
                true,
                Halt::Running,
                &mut memory,
            )
            .await;

            assert_eq!(
                sent::<IVault::recordValuationCall>(&node, VAULT),
                1,
                "ignores the block time: {ignores}"
            );
            assert_eq!(memory.valuations_refused.slots, 0);
            assert_eq!(
                simulated_at::<IVault::recordValuationCall>(&node, VAULT),
                if ignores {
                    vec![]
                } else {
                    vec![SLOT_START + 3]
                }
            );
        }
    }

    /// A node that refuses a call given a block time can't simulate a
    /// valuation at its landing: it goes on its gas estimate.
    #[tokio::test]
    async fn a_node_that_refuses_the_block_time_leaves_the_valuation_to_its_estimate() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY);
        crate::stand_in::refuses_block_time(&node);
        let client = connect(&node).await;

        price_at(
            &node,
            &client,
            SLOT_START,
            Halt::Running,
            &mut Memory::default(),
        )
        .await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 1);
    }

    /// Whether an endpoint runs a call at the time given is asked once, not
    /// before every valuation.
    #[tokio::test]
    async fn an_endpoint_is_asked_once_whether_it_runs_a_call_at_the_time_given() {
        let node = node(SLOT_START);
        a_quiet_vault(&node, SLOT_START - DAY);
        let client = connect(&node).await;
        let mut memory = Memory::default();

        price_at(&node, &client, SLOT_START, Halt::Running, &mut memory).await;
        price_at(&node, &client, SLOT_START + 10, Halt::Running, &mut memory).await;

        assert_eq!(sent::<IVault::recordValuationCall>(&node, VAULT), 2);
        assert_eq!(node.lock().unwrap().block_time_probes, 1);
    }

    #[test]
    fn payday_falls_on_the_monday_after_the_last_one_as_the_vault_counts_it() {
        // Monday 13 November 2023, 00:00 UTC.
        let monday = 1_699_833_600;
        assert_eq!(next_monday(monday - 1), monday);
        assert_eq!(next_monday(monday), monday + 7 * DAY);
        assert_eq!(next_monday(monday + 7 * DAY - 1), monday + 7 * DAY);
        assert_eq!(next_monday(0), 4 * DAY);
    }

    #[test]
    fn a_valuation_is_simulated_a_block_late_only_where_the_window_has_room_for_it() {
        // Two-second blocks in a three-second window.
        assert_eq!(landing_times(1_001, 3, 2), (1_003, 1_005));
        // One-second blocks.
        assert_eq!(landing_times(1_001, 3, 1), (1_002, 1_003));
        // Blocks longer than the window: only the block it lands in.
        assert_eq!(landing_times(1_000, 3, 3_600), (4_600, 4_600));
    }
}
