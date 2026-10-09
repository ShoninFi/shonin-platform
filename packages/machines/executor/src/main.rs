//! The attested executor.
//!
//! It finds the vaults that are open, makes a key for each one (in memory
//! only, from the operating system's randomness), attests to every key it
//! holds at once (see [`attestation`]), waits for the keeper to bless each key
//! and stock it with gas, has the vault's station approve that key at the
//! venue, and runs each vault's agent.
//! The key can trade and nothing else: the venue refuses a withdrawal signed by
//! it, and the station only moves money between the vault's own accounts.
//! Whatever an agent decides, that is the whole of what can happen.
//!
//! It also keeps a signed running total of what running the agent costs, for
//! the keeper to collect from the tank. Valuing the vault is the keeper's:
//! anyone may, and one loop doing it means no two race for the same slot.

mod attestation;
mod ceilings;
mod data;
mod feed;
mod handover;
mod health;
mod host;
mod hyperliquid;
mod inference;
mod keys;
mod net;
mod pace;
mod robinhood;
mod root;
mod runner;
mod scale;
mod solana;
mod state;
mod store;
mod streams_rh;
mod usage;
mod watch;

#[cfg(test)]
mod attested_path;
#[cfg(test)]
mod stand_in;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result};
use axum::response::IntoResponse;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStation;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{IVault, VaultState};
use chain::bindings::venue::ILocalVenue;
use chain::{Client, Deployments};
use clap::Parser;
use wasmtime::Engine;

#[derive(Parser)]
#[command(about = "Runs strategies for the vaults it is blessed for")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    /// Where bundles are, by hash.
    #[arg(long, env = "SHONIN_STORE")]
    store: PathBuf,
    /// Where the health reports go, and where another executor's are read.
    #[arg(long, env = "SHONIN_API_URL")]
    api_url: String,
    /// The status port, where each vault's signed total and the instance's
    /// attestation are served. 0 has the system pick one, said in the log as
    /// the executor starts; without it, nothing is served. A port that can't
    /// be bound stops the executor.
    #[arg(long)]
    port: Option<u16>,
    /// The address the status port listens on: 0.0.0.0 where the keeper and
    /// the prover reach it from other machines.
    #[arg(long, default_value = "127.0.0.1")]
    listen: IpAddr,
    /// How the instance attests to its keys: `launcher` in Confidential
    /// Space (a token from the launcher, proved by a prover), `development`
    /// on a development chain (sealed with the attestor role).
    #[arg(long, value_enum, default_value = "development")]
    attestation: attestation::Mode,
    /// The launcher's socket, where tokens are asked for.
    #[arg(long, default_value = attestation::LAUNCHER_SOCKET)]
    launcher_socket: PathBuf,
    /// How long a vault's approved key, still blessed and not this
    /// executor's, must send no health report before the vault is taken
    /// over.
    #[arg(long, default_value_t = handover::HANDOVER_SILENCE)]
    handover_silence_seconds: u64,
    /// The public copy of the store, read through for a file not on disk and
    /// believed only for its hash.
    #[arg(long, env = "SHONIN_STORE_URL")]
    store_url: Option<String>,
    /// The most of each allowance one agent is given here.
    #[command(flatten)]
    ceilings: ceilings::Ceilings,
    /// Hyperliquid's public WebSocket, which the shared Hyperliquid stream
    /// reads from: its testnet's unless the network is mainnet.
    #[arg(long, env = "SHONIN_HYPERLIQUID_STREAM")]
    hyperliquid_stream: Option<String>,
    /// Other holders of the root key (their status ports), asked for it by
    /// attested exchange when the canonical chain records a root this
    /// instance doesn't hold. Baked into the image; none on the testnet.
    #[arg(long, env = "SHONIN_ROOT_PEERS", value_delimiter = ',')]
    root_peers: Vec<String>,
    /// Hold no root key, whatever the network's chain config says: for a
    /// test that runs an executor beside one that does.
    #[arg(long, hide = true)]
    no_root_key: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();
    // One TLS crypto provider for the process, chosen here: a dependency
    // that leaves the choice to the process default panics when the build
    // has turned two on.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();
    data::configure(args.hyperliquid_stream.clone().unwrap_or_else(|| {
        if args.chain_args.network == chain::NetworkName::Mainnet {
            data::HYPERLIQUID_MAINNET
        } else {
            data::HYPERLIQUID_TESTNET
        }
        .to_string()
    }));
    // Agents name a spot pair's book as their orders name the pair; the
    // venue numbers pairs per network, so their feed names are read from it,
    // and read again now and then for pairs listed since.
    let pairs_network = if args.chain_args.network == chain::NetworkName::Mainnet {
        ::hyperliquid::Network::Mainnet
    } else {
        ::hyperliquid::Network::Testnet
    };
    tokio::spawn(async move {
        let api = ::hyperliquid::Api::new(pairs_network);
        let mut warned = Throttle::default();
        loop {
            let wait = match ::hyperliquid::Markets::read(&api).await {
                Ok(markets) => {
                    data::name_spot_pairs(&markets);
                    SPOT_PAIRS_READ_EVERY
                }
                Err(error) => {
                    if warned.due(std::time::Instant::now()) {
                        tracing::warn!(error = %format!("{error:#}"), "Hyperliquid's spot pairs weren't read; agents can't yet ask for a pair's book by its name");
                    }
                    Duration::from_secs(5)
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
    // The EVM chains and the Solana clusters to serve. A Solana vault chain
    // named alone is served alone.
    let solana_targets = args.chain_args.solana_targets()?;
    let evm_targets = if args.chain_args.vault_chain.is_some() && !solana_targets.is_empty() {
        Vec::new()
    } else {
        args.chain_args.targets()?
    };
    // Each chain is served as soon as it connects: one out of reach (its node
    // down, its deployment missing) is tried again in the background, and
    // doesn't keep the executor from the vaults on the others.
    let connecting = if evm_targets.is_empty() {
        None
    } else {
        Some(args.chain_args.connect_each()?)
    };
    let mut connecting_solana = if solana_targets.is_empty() {
        None
    } else {
        Some(args.chain_args.connect_each_solana()?)
    };
    // The instance's key and the tree of every vault key it holds. In
    // Confidential Space the token is asked for, and the seal posted, on the
    // status port, so there has to be one.
    anyhow::ensure!(
        args.attestation == attestation::Mode::Development || args.port.is_some(),
        "attesting through the launcher needs the status port (--port)"
    );
    let attestation = attestation::Attestation::new(args.attestation)?;
    tokio::spawn(attestation.clone().run(args.launcher_socket.clone()));
    tracing::info!(
        mode = ?args.attestation,
        instance = %attestation.instance(),
        "the executor is up"
    );

    // The root key, on the network's canonical chain where it names one that
    // this executor serves: the service is on the status port from the
    // start, and reads the chain once it is connected.
    let root_chain = if args.no_root_key {
        None
    } else {
        args.chain_args
            .root_chain()?
            .filter(|key| evm_targets.iter().any(|target| target.key.same_chain(key)))
            .and_then(|key| key.evm_chain_id())
    };
    let root_service = root_chain.map(|chain_id| {
        // Until the holder has looked at the record, it may be about to make
        // a root: the first token waits for that, a bounded while.
        attestation.await_root(attestation::ROOT_DECIDED_WITHIN);
        let service = shonin_rootkey::Service::new(shonin_rootkey::NetworkView::new(chain_id));
        // Google's keys check a token only inside Confidential Space, where
        // there is a launcher to give one.
        if args.attestation == attestation::Mode::Launcher {
            tokio::spawn(root::keep_google_keys(service.clone()));
        }
        service
    });
    if root_chain.is_none() {
        tracing::info!(
            "no canonical chain for the root key is served here: this executor holds no root"
        );
    }

    let totals: usage::Totals = Default::default();
    let solana_totals: solana::Totals = Default::default();
    // Every vault chain to be served, each said connected as it connects: the
    // status port's health is unavailable until all of them have.
    let served_chains = health::Chains::new(
        evm_targets
            .iter()
            .chain(&solana_targets)
            .map(|target| target.vault_chain)
            .collect(),
    );
    if let Some(port) = args.port {
        let listener = bind_status(SocketAddr::new(args.listen, port)).await?;
        tracing::info!(
            port = listener.local_addr()?.port(),
            "the status port is bound"
        );
        tokio::spawn(serve_status(
            listener,
            totals.clone(),
            solana_totals.clone(),
            served_chains.clone(),
            attestation.clone(),
            root_service.clone(),
        ));
    }

    let engine = runner::engine()?;
    runner::start_epoch_ticker(&engine);

    // Each vault chain is swept on its own, with its own vaults: the same
    // address can be a different vault on another chain.
    // How deep each chain's vaults are read for what they have collected.
    let confirmations: HashMap<shared::Chain, u64> = evm_targets
        .iter()
        .map(|target| {
            let deep = target.config.confirmations.unwrap_or(CONFIRMATIONS);
            (target.vault_chain, deep)
        })
        .collect();
    // The price of each Solana cluster's coin, from its config.
    let native_usd: HashMap<shared::Chain, Option<f64>> = solana_targets
        .iter()
        .map(|target| (target.vault_chain, target.config.native_usd))
        .collect();
    let args = Arc::new(args);
    let mut chains = tokio::task::JoinSet::new();
    if let Some(mut connecting_solana) = connecting_solana.take() {
        let engine = engine.clone();
        let args = args.clone();
        let served_chains = served_chains.clone();
        let attestation = attestation.clone();
        chains.spawn(async move {
            let mut clusters = tokio::task::JoinSet::new();
            while let Some(served) = connecting_solana.recv().await {
                served_chains.connected(served.vault_chain);
                let native_usd = native_usd.get(&served.vault_chain).copied().flatten();
                clusters.spawn(solana::run(
                    served,
                    native_usd,
                    engine.clone(),
                    args.clone(),
                    solana_totals.clone(),
                    attestation.clone(),
                ));
            }
            while clusters.join_next().await.is_some() {}
        });
    }
    let Some(mut connecting) = connecting else {
        while chains.join_next().await.is_some() {}
        return Ok(());
    };
    while let Some(served) = connecting.recv().await {
        served_chains.connected(served.vault_chain);
        attestation.serve_chain(served.client.clone(), served.deployments.clone());
        // Every chain served has its registry read by the root key service:
        // a vault key is blessed on its vault's own chain, so that is where
        // the exchange reads it. The canonical chain's also answers for the
        // asker's image, project and checkers.
        if let Some(service) = &root_service {
            service.view().connect(shonin_rootkey::RegistryView::new(
                served.client.clone(),
                served.deployments.attestation,
                served.deployments.chain_id,
            ));
        }
        if let (Some(service), Some(chain_id)) = (&root_service, root_chain)
            && served.deployments.chain_id == chain_id
        {
            let holder = root::Holder::new(
                service.clone(),
                attestation.clone(),
                chain_id,
                root::OnChain {
                    client: served.client.clone(),
                    deployments: served.deployments.clone(),
                },
                root::Exchange {
                    urls: args.root_peers.clone(),
                    mode: args.attestation,
                    socket: args.launcher_socket.clone(),
                    http: reqwest::Client::new(),
                },
            );
            tokio::spawn(holder.run());
        }
        let attestation = attestation.clone();
        let engine = engine.clone();
        let args = args.clone();
        let totals = totals.clone();
        let confirmations = confirmations
            .get(&served.vault_chain)
            .copied()
            .unwrap_or(CONFIRMATIONS);
        chains.spawn(async move {
            let mut book = Book {
                confirmations,
                ..Book::default()
            };
            let mut unswept = Throttle::default();
            loop {
                if let Err(error) = sweep(
                    served.vault_chain,
                    &served.client,
                    &served.deployments,
                    served.native_usd,
                    &engine,
                    &args,
                    &totals,
                    &attestation,
                    &mut book,
                )
                .await
                    && unswept.due(Instant::now())
                {
                    tracing::warn!(chain = %served.key, %error, "could not look for work");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
    while chains.join_next().await.is_some() {}
    Ok(())
}

/// The capability a manifest declares to be given requests and WebSockets
/// to any host.
pub const NETWORK: &str = "network";

/// How many blocks deep a vault's collected figure is read, on a chain whose
/// config gives none.
const CONFIRMATIONS: u64 = 12;

/// How often the factory's list of vaults is read again. Each vault already
/// known is still looked at every sweep.
const VAULT_LIST_EVERY: Duration = Duration::from_secs(30);

/// What a chain's sweep keeps from one sweep to the next.
#[derive(Default)]
struct Book {
    /// How many blocks deep the chain's vaults are read for what they have
    /// collected (see [`usage`]).
    confirmations: u64,
    /// Every vault the factory has made, as last read, and when.
    vaults: Vec<Address>,
    vaults_read: Option<Instant>,
    /// The vaults taken on.
    running: HashMap<Address, Served>,
    /// Each vault's home station, read once: a vault's station is set when
    /// it is made and never changes.
    stations: HashMap<Address, Address>,
    /// The vaults waiting for their bundle to reach the store.
    waiting_for_bundle: HashSet<Address>,
    /// The vaults whose bundle and manifest have been read through from the
    /// public store (or found on disk), so they aren't asked for again.
    read_through: HashSet<Address>,
    /// Vaults that could not be taken on, and when to try again: held off
    /// for longer each time rather than tried at every sweep.
    taking_on: HashMap<Address, Retry>,
    /// When each vault was last warned about, so a chain that is down
    /// doesn't write the same line for every vault at every sweep.
    warned: HashMap<Address, Throttle>,
    /// Vaults let go because another executor has taken them over, or
    /// because the station refuses to approve this executor's key: not taken
    /// on again while the other executor's key, still blessed, holds the
    /// station's approval, nor while the station would refuse this key.
    replaced: HashSet<Address>,
    /// Vaults this executor never held whose station approves another
    /// executor's key, still blessed: watched for that key's health reports,
    /// and taken over once it has been silent long enough (see [`handover`]).
    watching: HashMap<Address, handover::Watch>,
    /// Vaults being let go because the key this executor was taking them over
    /// from reported again: once let go, watched afresh rather than counted
    /// as replaced.
    rewatch: HashSet<Address>,
}

impl Book {
    /// Whether a warning about this vault is due.
    fn warn(&mut self, vault: Address) -> bool {
        self.warned.entry(vault).or_default().due(Instant::now())
    }
}

/// Lets a warning through at most once a minute, so a failure that lasts is
/// said again now and then rather than at every look.
#[derive(Default)]
pub struct Throttle(Option<Instant>);

impl Throttle {
    const EVERY: Duration = Duration::from_secs(60);

    pub fn due(&mut self, now: Instant) -> bool {
        let due = self
            .0
            .is_none_or(|said| now.duration_since(said) >= Self::EVERY);
        if due {
            self.0 = Some(now);
        }
        due
    }
}

/// One vault this executor has taken on. Its key, its meter and its reports
/// live as long as the vault does, however often the agent itself has to be
/// started again: two meters for one vault would sign two totals, the later
/// one short of the first.
struct Served {
    shared: Shared,
    serving: Option<tokio::task::JoinHandle<Ended>>,
    retry: Retry,
    /// The meter and the reports, which run for as long as the vault is
    /// served: a halted vault's final bill is signed after its grace. The
    /// reports stop as soon as the vault is found held by another executor
    /// (see [`Served::stop_reports`]).
    _upkeep: [AbortOnDrop; 2],
    /// The vault's key in the instance's tree, for as long as the vault is
    /// held: its leaf leaves the tree when the vault is let go.
    _leaf: Leaf,
    /// When the agent stopped for good because this executor no longer holds
    /// the vault (see [`Stop::Replaced`]). The vault is let go once every
    /// send of the agent's has landed or been given up on and the meter has
    /// signed a total counting up to then and to the stop, so the last bill,
    /// the gas of the agent's last orders included, can still be collected;
    /// never later than [`LAST_SENDS_LAND_WITHIN`] after the stop.
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
}

/// A vault's key in the instance's tree (see [`attestation`]), let go with
/// the vault.
struct Leaf {
    attestation: attestation::Attestation,
    chain_id: u64,
    vault: Address,
    key: Address,
}

impl Drop for Leaf {
    fn drop(&mut self) {
        self.attestation.remove(self.chain_id, self.vault, self.key);
    }
}

/// A vault let go is let go entirely: the agent it was running stops with
/// its meter and its reports, rather than trading on for a vault nobody
/// serves any more.
impl Drop for Served {
    fn drop(&mut self) {
        if let Some(serving) = &self.serving {
            serving.abort();
        }
    }
}

/// What the meter, the reports and the agent share for one vault.
#[derive(Clone)]
struct Shared {
    /// The agent's key. What it spends on gas is the author's bill.
    key: chain::KeyClient,
    /// The same key, made in the enclave for this vault alone, for the
    /// halves that sign with it themselves.
    vault_key: Arc<keys::Key>,
    /// The instance's attestation, where the key's leaf is and whether it
    /// wants blessing is noted.
    attestation: attestation::Attestation,
    /// Another executor's key, still blessed, that this one takes the vault
    /// over from once its own key is blessed: the station's approval is
    /// taken from that key, and from no other, and only while it is still
    /// silent. Cleared once this key holds the approval; one that replaces it
    /// after that ends the serve.
    takes_over: Arc<Mutex<Option<handover::Takeover>>>,
    /// The API a takeover asks, before taking the approval, whether the key
    /// it takes over from is still silent.
    api_url: String,
    /// Whether this serve holds the station's approval with the key, which
    /// alone lets the key's reports go.
    holds: health::Holds,
    /// Whether the tank is dry, as the meter last found it.
    dry: Arc<AtomicBool>,
    /// Whether the meter has read the tank at all yet. Until it has, nobody
    /// knows the tank isn't dry already, so the agent waits.
    metered: Arc<AtomicBool>,
    /// Whether the agent is running at this moment: the meter bills
    /// capacity only for the time it is.
    running: Arc<AtomicBool>,
    counters: Arc<Mutex<host::Counters>>,
    /// The next block the vault's events are to be read from: the head when
    /// the vault was taken on, until a watcher reads on from there. A watcher
    /// started again after a restart carries on from here, so nothing that
    /// happened in between is missed.
    watched_to: Arc<AtomicU64>,
    /// What the watcher has read and the agent is yet to. They outlive any
    /// one agent, since a block is marked read once its events are here: an
    /// agent that stops before reading them leaves them to the next.
    account_events: feed::Feed<Vec<u8>>,
    vault_events: feed::Feed<watch::VaultEvent>,
    /// Whether the agent has wound down in the vault's halt: it ended without
    /// a fault after it was told the vault is halting. Kept with the vault,
    /// so a serve started again (after a read that failed while the grace
    /// was waited out) doesn't start an agent that has done its part.
    wound_down: Arc<AtomicBool>,
    /// The moment the meter's latest signed total counts up to.
    signed_through: usage::SignedThrough,
    /// The key's last approval at the venue that may still land: noted by
    /// the send itself when it is broadcast, and kept with the vault, so a
    /// serve started again (the last one stopped during that very send)
    /// doesn't send another while the first may still land: both would
    /// land, each paid for.
    approvals: chain::Unanswered,
    /// The most of each allowance the agent is given here.
    ceilings: ceilings::Ceilings,
    /// Starts in a row that failed, the agent's own faults and the serves'
    /// failures alike: kept with the vault, so a serve that ends (a read
    /// failing between one fault and the next start) doesn't start the
    /// count again. Only an agent that runs for [`RAN_FOR_A_WHILE`] does.
    failed_starts: Arc<AtomicU32>,
    /// Whether an agent has been handed the vault's halting notice. Until
    /// one has, a halted vault cuts every held-off restart short, the sweep's
    /// (a serve that failed) as the serve's own: a halt that cut one wait
    /// short and was then never heard of (the serve failed again before the
    /// agent read it) still cuts the next one short, though never sooner
    /// than [`HALT_RESTART_FLOOR`] into it.
    halt_known: Arc<AtomicBool>,
}

/// How a serve ended.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ended {
    failed: bool,
    /// Whether it ended because another executor's key, still blessed, holds
    /// the station's approval.
    replaced: bool,
    /// Whether it ended because the key it was taking the vault over from
    /// reported again.
    resumed: bool,
}

/// Why a serve that didn't fail ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stop {
    /// The vault has nothing left to run.
    Finished,
    /// Another executor's key, still blessed, holds the station's approval:
    /// one executor at a time, so this one stops. Or the station refuses to
    /// approve this key (see [`refused`]), and this one stops the same way,
    /// its agent unable to trade.
    Replaced,
    /// The key this one was taking the vault over from reported again before
    /// this key took the approval: the vault is left to it, and its silence
    /// watched afresh.
    Resumed,
}

/// Starts in a row that failed, and when the next may start.
#[derive(Default)]
struct Retry {
    /// For a vault taken on, shared with its serves (see
    /// [`Shared::failed_starts`]).
    failures: Arc<AtomicU32>,
    at: Option<Instant>,
    /// When the wait began.
    began: Option<Instant>,
    /// What the host would have held the agent to of the allowances when
    /// the wait began, if they could be read then: a change from these cuts
    /// the wait short.
    under: Option<shared::Allowances>,
}

impl Retry {
    /// Take note of how the last serve ended, and say whether it counts as
    /// a restart. One that failed does, and the next is held off for longer
    /// each time, so a vault that can't be served (its blessing refused, its
    /// chain down) isn't retried every sweep at the platform's expense. Only
    /// an agent that ran for a good while starts the count again, never the
    /// way a serve ends.
    fn ended(&mut self, ended: Ended, now: Instant) -> bool {
        if ended.failed {
            let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
            self.at = Some(now + backoff(failures));
            self.began = Some(now);
        } else {
            self.at = None;
            self.began = None;
        }
        ended.failed
    }

    fn waiting(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now < at)
    }
}

/// The soonest a held-off restart is cut short by a halt nobody has heard
/// of: a halt cuts waits short until an agent is told of it, and an agent
/// that faults before it is told would otherwise be started again every few
/// seconds for the whole grace.
const HALT_RESTART_FLOOR: Duration = Duration::from_secs(10);

/// How long to hold off after this many failed serves in a row: five
/// seconds, doubling, never more than five minutes.
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    (Duration::from_secs(5) * 2u32.pow(doublings)).min(Duration::from_secs(300))
}

/// Whether every send of an agent stopped at `stopped` has landed or been
/// given up on, and a total counting up to then has been signed. The
/// agent's sends still landing add their gas to the key's count, which only
/// a total signed after they land bills.
fn last_sends_billed(served: &Served, stopped: Instant) -> bool {
    let key = &served.shared.key;
    let through = key.last_settled().map_or(stopped, |at| at.max(stopped));
    !key.sends_in_flight() && served.shared.signed_through.covers(through)
}

/// The longest a vault let go (closed, or held by another executor) is kept
/// for its agent's last sends: as long as a send's receipt is looked for,
/// and two minutes for the total after.
const LAST_SENDS_LAND_WITHIN: Duration =
    Duration::from_secs(chain::client::LANDING_TIMEOUT.as_secs() + 2 * 60);

/// Find the vaults that are open and have a bundle, and start anything that
/// is not already running. A vault that can't be read this sweep is said at
/// `warn` and left for the next; it holds up no other.
#[allow(clippy::too_many_arguments)]
async fn sweep(
    chain: shared::Chain,
    client: &Client,
    deployments: &Deployments,
    native_usd: Option<f64>,
    engine: &Engine,
    args: &Args,
    totals: &usage::Totals,
    attestation: &attestation::Attestation,
    book: &mut Book,
) -> Result<()> {
    // The factory's list of every vault it ever made is read whole, so it is
    // read again only every so often: a vault made since is taken on within
    // [`VAULT_LIST_EVERY`]. While a fresh read fails, the vaults already
    // known are swept on, and the failure is said once they have been.
    let mut listing = Ok(());
    if book
        .vaults_read
        .is_none_or(|at| at.elapsed() >= VAULT_LIST_EVERY)
    {
        match client
            .read(deployments.factory, IVaultFactory::vaultsCall {})
            .await
        {
            Ok(vaults) => {
                book.vaults = vaults;
                book.vaults_read = Some(Instant::now());
            }
            Err(error) if book.vaults_read.is_none() => return Err(error),
            Err(error) => listing = Err(error),
        }
    }

    for vault in book.vaults.clone() {
        if let Err(error) = sweep_one(
            chain,
            client,
            deployments,
            native_usd,
            engine,
            args,
            totals,
            attestation,
            book,
            vault,
        )
        .await
            && book.warn(vault)
        {
            tracing::warn!(%chain, vault = %vault, error = format!("{error:#}"), "could not look at this vault");
        }
    }
    listing
}

/// One vault's part of a sweep.
#[allow(clippy::too_many_arguments)]
async fn sweep_one(
    chain: shared::Chain,
    client: &Client,
    deployments: &Deployments,
    native_usd: Option<f64>,
    engine: &Engine,
    args: &Args,
    totals: &usage::Totals,
    attestation: &attestation::Attestation,
    book: &mut Book,
    vault: Address,
) -> Result<()> {
    let state = client.read(vault, IVault::stateCall {}).await?;
    // A closed vault has nothing left to run, and is let go as a vault
    // another executor has taken over is: its agent stopped, its key no
    // longer kept in gas, and the meter kept until the agent's last sends
    // have landed and a total counting their gas is signed, which the
    // keeper collects in the closed vault's last bill. Never longer than
    // those sends can take to land.
    if matches!(state, VaultState::Closed | VaultState::Cancelled) {
        if let Some(served) = book.running.get_mut(&vault) {
            let stopped = *served.letting_go.get_or_insert_with(|| {
                tracing::info!(%chain, vault = %vault, "the vault has closed; letting it go once the agent's last sends are billed");
                if let Some(serving) = served.serving.take() {
                    serving.abort();
                }
                Instant::now()
            });
            if !last_sends_billed(served, stopped) && stopped.elapsed() < LAST_SENDS_LAND_WITHIN {
                return Ok(());
            }
            book.running.remove(&vault);
            tracing::info!(%chain, vault = %vault, "the vault has closed; let go");
        }
        book.waiting_for_bundle.remove(&vault);
        book.taking_on.remove(&vault);
        book.warned.remove(&vault);
        book.replaced.remove(&vault);
        book.rewatch.remove(&vault);
        book.watching.remove(&vault);
        return Ok(());
    }
    if let Some(served) = book.running.get_mut(&vault) {
        if served
            .serving
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Ok(());
        }
        if let Some(task) = served.serving.take() {
            let ended = task.await.unwrap_or(Ended {
                failed: true,
                replaced: false,
                resumed: false,
            });
            if ended.resumed {
                // The key this executor was taking the vault over from is
                // heard from again: its own key never took the approval, and
                // the vault is left to the other, its silence watched afresh.
                tracing::info!(%chain, vault = %vault, "the key taken over from reported again; letting the vault go and watching it afresh");
                served.stop_reports();
                served.letting_go = Some(Instant::now());
                book.rewatch.insert(vault);
            } else if ended.replaced {
                // Another executor has taken the vault over, or the station
                // refuses this key. Its agent has stopped and its key is no
                // longer attested once the vault is let go. The meter signs
                // once more, for the time up to now and the agent's last
                // sends.
                tracing::info!(%chain, vault = %vault, "this executor no longer holds the vault's approval; letting it go");
                served.stop_reports();
                served.letting_go = Some(Instant::now());
            } else if served.retry.ended(ended, Instant::now()) {
                if let Ok(mut counters) = served.shared.counters.lock() {
                    counters.restarts += 1;
                }
                // Read as the wait begins, not taken from whatever the last
                // agent ran under: a serve that failed before it read them
                // would leave a change behind for every sweep to find again.
                served.retry.under = held_to(client, vault, served.shared.ceilings).await;
            }
        }
        if let Some(stopped) = served.letting_go {
            if last_sends_billed(served, stopped) || stopped.elapsed() >= LAST_SENDS_LAND_WITHIN {
                book.running.remove(&vault);
                if !book.rewatch.remove(&vault) {
                    book.replaced.insert(vault);
                }
            }
            return Ok(());
        }
        if served.retry.waiting(Instant::now()) {
            // A restart held off here, after a serve that failed, is cut
            // short by what cuts one the serve holds off short (see
            // [`hold_off`]), and the serve that follows acts on it at once.
            let shared = &served.shared;
            let station = match book.stations.get(&vault) {
                Some(station) => *station,
                None => {
                    let station = client.read(vault, IVault::homeStationCall {}).await?;
                    book.stations.insert(vault, station);
                    station
                }
            };
            let under = served.retry.under;
            let now = look_held(
                client,
                deployments,
                HeldOff {
                    vault,
                    state,
                    station,
                    key: shared.key.address,
                },
                under.is_some().then_some(shared.ceilings),
            )
            .await;
            let halt_known = shared.halt_known.load(Ordering::SeqCst);
            let waited = served
                .retry
                .began
                .map_or(Duration::MAX, |began| began.elapsed());
            let Some(woke) = woken(under.as_ref(), halt_known, waited, &now) else {
                return Ok(());
            };
            tracing::info!(%chain, vault = %vault, ?woke, "a held-off restart was cut short");
            served.retry.at = None;
        }
    }
    // A retired vault still runs until it is evicted. A killed or frozen one
    // runs through its halt's grace, so an agent started again in the grace
    // (the executor restarted, a serve that failed) is told it is halting and
    // has the rest of the grace to wind down in.
    let servable = match state {
        VaultState::Open | VaultState::WindingDown => true,
        VaultState::Killed | VaultState::Frozen => {
            client
                .read(
                    vault,
                    chain::bindings::station::IStationVault::haltedAtCall {},
                )
                .await?
                != 0
        }
        _ => false,
    };
    if !servable {
        return Ok(());
    }
    if finished(client, vault).await? {
        // Past its grace the vault's station is stopped and its key has
        // nothing left to send. The meter runs on, so the final bill can
        // still be collected.
        return Ok(());
    }
    let bundle = client.read(vault, IVault::bundleHashCall {}).await?;
    if bundle == B256::ZERO {
        return Ok(());
    }
    let mut component = bundle_path(&args.store, bundle);
    // A vault's files not on disk are read through from the public store, if
    // there is one, and kept only once they check against the hashes the
    // chain holds: its bundle, and its own manifest, which a vault on another
    // chain running the same bundle doesn't share. Once a vault's files are
    // all on disk it isn't asked again.
    if (!component.exists() || !book.read_through.contains(&vault))
        && let Some(base) = args.store_url.as_deref()
    {
        match fetch_bundle(client, deployments, &args.store, base, vault, bundle).await {
            Ok(true) => {
                book.read_through.insert(vault);
            }
            Ok(false) => {}
            Err(error) => {
                if book.warn(vault) {
                    tracing::warn!(%chain, vault = %vault, %bundle, error = format!("{error:#}"), "could not read the bundle through from the public store");
                }
            }
        }
        component = bundle_path(&args.store, bundle);
    }
    if !component.exists() {
        // Said once, not every sweep, so an operator sees why a vault isn't
        // running without the log filling with it.
        if book.waiting_for_bundle.insert(vault) {
            tracing::info!(%chain, vault = %vault, %bundle, "no bundle in the store yet; the vault waits for it");
        }
        return Ok(());
    }
    book.waiting_for_bundle.remove(&vault);

    if !book.running.contains_key(&vault) {
        if book
            .taking_on
            .get(&vault)
            .is_some_and(|retry| retry.waiting(Instant::now()))
        {
            return Ok(());
        }
        // One executor at a time: a vault whose station approves another
        // executor's key, still blessed, is that executor's while it is
        // heard from. This executor holds no key for the vault yet, so where
        // the approval stands is asked for no key of its own.
        let station = client.read(vault, IVault::homeStationCall {}).await?;
        let approval =
            approval_of(client, deployments, station, Address::ZERO, vault, bundle).await?;
        let mut takes_over = None;
        if let Approval::Replaced(by) = approval {
            // A vault this executor was replaced on is never taken back while
            // the key that replaced it is blessed.
            if book.replaced.contains(&vault) {
                return Ok(());
            }
            let now = unix_now();
            let watch = book
                .watching
                .entry(vault)
                .or_insert_with(|| {
                    tracing::info!(%chain, vault = %vault, %by, "another executor's key holds the approval; watching for its reports");
                    handover::Watch::new(by, now)
                });
            if watch.by != by {
                tracing::info!(%chain, vault = %vault, %by, "another key holds the approval now; watching for its reports");
                *watch = handover::Watch::new(by, now);
            }
            if !watch.look_due(Instant::now()) {
                return Ok(());
            }
            match handover::latest_report(&HTTP, &args.api_url, chain, vault).await {
                Ok(latest) => {
                    if watch.heard(latest, unix_now()) == handover::Heard::Report
                        || !watch.silent_for(args.handover_silence_seconds, unix_now())
                    {
                        return Ok(());
                    }
                }
                Err(error) => {
                    if book.warn(vault) {
                        tracing::warn!(%chain, vault = %vault, error = format!("{error:#}"), "could not read the vault's latest report; the watch goes on");
                    }
                    return Ok(());
                }
            }
            tracing::info!(%chain, vault = %vault, %by, silence = args.handover_silence_seconds, "the approved key has sent no report for the whole watch; taking the vault over");
            takes_over = Some(handover::Takeover::new(by, unix_now()));
        }
        book.watching.remove(&vault);
        // A vault let go is taken on again only once the station would
        // approve a key of this executor's: taking it on asks for a blessing.
        if book.replaced.contains(&vault)
            && refused(
                client,
                deployments,
                attestation,
                station,
                Address::ZERO,
                vault,
                bundle,
            )
            .await?
        {
            return Ok(());
        }
        if book.replaced.remove(&vault) {
            tracing::info!(%chain, vault = %vault, "no other executor's key holds the approval and the station would approve one of this executor's; taking the vault on again");
        }
        // The vault's events are read from here on: an agent held back for
        // its blessing, its gas or its tank still hears what happened since.
        let taken = match client.block_number().await {
            Ok(head) => take_on(
                client,
                chain,
                deployments,
                native_usd,
                vault,
                bundle,
                head,
                takes_over,
                args,
                totals,
                attestation,
                book.confirmations,
            ),
            Err(error) => Err(error),
        };
        match taken {
            Ok(served) => {
                tracing::info!(%chain, vault = %vault, key = %served.shared.key.address, "taking on a vault with a key of its own");
                book.taking_on.remove(&vault);
                book.running.insert(vault, served);
            }
            Err(error) => {
                book.taking_on.entry(vault).or_default().ended(
                    Ended {
                        failed: true,
                        replaced: false,
                        resumed: false,
                    },
                    Instant::now(),
                );
                if book.warn(vault) {
                    tracing::warn!(%chain, vault = %vault, error = format!("{error:#}"), "could not take on this vault");
                }
                return Ok(());
            }
        }
    }
    let Some(served) = book.running.get_mut(&vault) else {
        return Ok(());
    };

    let client = client.clone();
    let deployments = deployments.clone();
    let engine = engine.clone();
    let store = args.store.clone();
    let network = args.chain_args.network.to_string();
    let shared = served.shared.clone();
    served.serving = Some(tokio::spawn(async move {
        let served = serve(
            client,
            chain,
            deployments,
            engine,
            vault,
            bundle,
            store,
            network,
            shared,
        )
        .await;
        if let Err(error) = &served {
            tracing::warn!(%chain, vault = %vault, error = format!("{error:#}"), "stopped serving this vault");
        }
        Ended {
            failed: served.is_err(),
            replaced: matches!(served, Ok(Stop::Replaced)),
            resumed: matches!(served, Ok(Stop::Resumed)),
        }
    }));
    Ok(())
}

/// The HTTP client the sweep asks the API with.
static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
});

/// The time in unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Read a vault's bundle through from the public store: its recorded
/// manifest, checked against the hash the registry holds, then its
/// component, checked against the hash that manifest records.
/// Says whether the vault's manifest and component are both on disk now.
async fn fetch_bundle(
    client: &Client,
    deployments: &Deployments,
    store_dir: &std::path::Path,
    base: &str,
    vault: Address,
    bundle: B256,
) -> Result<bool> {
    let strategy_id = client.read(vault, IVault::strategyIdCall {}).await?;
    let registered = client
        .read(
            deployments.strategies,
            IStrategyRegistry::strategyOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let manifest_hash = registered.manifestHash;
    if !store::read_through(
        store_dir,
        Some(base),
        manifest_hash,
        "manifest.json",
        store::manifest_check(manifest_hash),
    )
    .await?
    {
        return Ok(false);
    }
    let path = store::path(store_dir, manifest_hash, "manifest.json");
    let canonical: shared::Canonical = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .context("the recorded manifest")?;
    anyhow::ensure!(
        canonical.bundle_hash == format!("sha256:{}", alloy::hex::encode(bundle)),
        "the recorded manifest is for another bundle"
    );
    store::read_through(
        store_dir,
        Some(base),
        bundle,
        "component.wasm",
        store::component_check(canonical.component_hash),
    )
    .await
}

/// How often a key waiting for its blessing or its gas is looked at: often
/// enough that a key the keeper has just blessed and stocked is approved at
/// the station within seconds, well inside its blessing.
const GAS_LOOK: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(2)
};

/// Make a vault's key, put it in the instance's tree, and start what runs
/// for as long as the vault does: its meter and its reports. Its events are
/// read from `head` on. `takes_over` is the other executor's key whose
/// approval this one takes, once its own key is blessed.
#[allow(clippy::too_many_arguments)]
fn take_on(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    native_usd: Option<f64>,
    vault: Address,
    bundle: B256,
    head: u64,
    takes_over: Option<handover::Takeover>,
    args: &Args,
    totals: &usage::Totals,
    attestation: &attestation::Attestation,
    confirmations: u64,
) -> Result<Served> {
    // The key is made here from the operating system's randomness, held in
    // memory only and never written down: an executor that restarts makes
    // a new one, and takes the vault over from its old self.
    let vault_key = Arc::new(keys::Key::fresh()?);
    let key = client.with_key(vault_key.signer())?;
    attestation.put(deployments.chain_id, vault, bundle, key.address);
    let leaf = Leaf {
        attestation: attestation.clone(),
        chain_id: deployments.chain_id,
        vault,
        key: key.address,
    };

    // What running it costs, kept as one signed total, and whether the tank
    // can still pay for it.
    let dry = Arc::new(AtomicBool::new(false));
    let metered = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(false));
    let signed_through = usage::SignedThrough::default();
    let failed_starts = Arc::new(AtomicU32::new(0));
    let metering = tokio::spawn(usage::run(
        client.clone(),
        key.clone(),
        vault,
        native_usd,
        totals.clone(),
        usage::Tank {
            dry: dry.clone(),
            metered: metered.clone(),
            signed_through: signed_through.clone(),
        },
        running.clone(),
        args.ceilings,
        confirmations,
    ));

    // A strategy's `run` is meant never to return, so the report cannot wait
    // for it: the counters are shared and sent while it is still running, and
    // they outlive a restart so that restarts can be counted at all.
    let counters = Arc::new(Mutex::new(host::Counters::default()));
    let holds = health::Holds::default();
    let mut reporter = health::Reporter::new(
        client.clone(),
        vault_chain,
        deployments.attestation,
        key.clone(),
        vault,
        args.api_url.clone(),
        holds.clone(),
        dry.clone(),
        totals.clone(),
        args.ceilings,
    );
    let reporting = counters.clone();
    let reports = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let now = reporting
                .lock()
                .map(|counters| *counters)
                .unwrap_or_default();
            reporter.post(now).await;
        }
    });

    Ok(Served {
        shared: Shared {
            key,
            vault_key,
            attestation: attestation.clone(),
            takes_over: Arc::new(Mutex::new(takes_over)),
            api_url: args.api_url.clone(),
            holds,
            dry,
            metered,
            running,
            counters,
            watched_to: Arc::new(AtomicU64::new(head)),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through,
            approvals: Default::default(),
            ceilings: args.ceilings,
            failed_starts: failed_starts.clone(),
            halt_known: Default::default(),
        },
        serving: None,
        retry: Retry {
            failures: failed_starts,
            ..Retry::default()
        },
        _upkeep: [AbortOnDrop(metering), AbortOnDrop(reports)],
        _leaf: leaf,
        letting_go: None,
    })
}

/// Run one vault's strategy, restarting it if it faults, until the vault has
/// nothing left to run or another executor takes it over.
#[allow(clippy::too_many_arguments)]
async fn serve(
    chain: Client,
    vault_chain: shared::Chain,
    deployments: Deployments,
    engine: Engine,
    vault: Address,
    bundle: B256,
    store: PathBuf,
    network: String,
    shared: Shared,
) -> Result<Stop> {
    let Shared {
        key,
        vault_key,
        attestation,
        takes_over,
        api_url,
        holds,
        dry,
        metered,
        running,
        counters,
        watched_to,
        account_events,
        vault_events,
        wound_down,
        signed_through: _,
        approvals,
        ceilings,
        failed_starts,
        halt_known,
    } = shared;
    // The venue's approval is the platform's doing, not the agent's, so its
    // gas is kept off the author's bill. Each send of it is noted as one that
    // may still land from the moment it is broadcast, by the send itself,
    // which carries on if this serve stops.
    let platform = key.unmetered();
    let approving = platform.noting(&approvals);
    let takeover = || takes_over.lock().ok().and_then(|from| *from);
    let taking_over = || takeover().map(|takeover| takeover.from);

    // One executor at a time: a vault another executor's key has taken over,
    // that key still blessed, is not served; unless this executor is taking
    // the vault over from that very key, after its silence.
    let station = chain.read(vault, IVault::homeStationCall {}).await?;
    if let Approval::Replaced(by) =
        approval_of(&chain, &deployments, station, key.address, vault, bundle).await?
        && taking_over() != Some(by)
    {
        tracing::info!(%vault, key = %key.address, %by, "another executor's key holds the approval; not serving");
        return Ok(Stop::Replaced);
    }

    // The key's leaf is in the instance's tree; the keeper blesses it from
    // the instance's seal, and stocks it with gas once the chain shows it
    // blessed. Nothing is sent until both have happened.
    attestation.put(deployments.chain_id, vault, bundle, key.address);
    wait_blessed_and_stocked(
        &chain,
        &deployments,
        &attestation,
        key.address,
        vault,
        bundle,
    )
    .await?;

    // The station is the venue account's master and approves the key there.
    // Anyone may ask; it obeys only what the registry says. The venue holds
    // the key approved only until the blessing the station last copied to
    // it, so a key blessed again is approved again, or the venue would turn
    // its orders away from the old blessing's end. A key another executor's
    // still-blessed key has replaced doesn't take the approval back: the
    // serve ends, and the vault is let go. So does one the station refuses.
    // While an approval an earlier serve sent, unanswered, may still land,
    // none is sent: where the approval stands is looked at until that one is
    // seen, or can no longer land.
    while approvals.may_still_land(key.address)
        && approval_of(&chain, &deployments, station, key.address, vault, bundle).await?
            == Approval::Behind
    {
        tokio::time::sleep(LANDING_LOOK).await;
    }
    // A takeover waits for its key's blessing, which can take a while: the
    // key it takes over from, still in place, is looked at once more, and
    // one heard from again meanwhile keeps the vault. Its silence is then
    // watched afresh.
    if let Some(takeover) = takeover()
        && approval_of(&chain, &deployments, station, key.address, vault, bundle).await?
            == Approval::Replaced(takeover.from)
    {
        let latest = handover::latest_report(&HTTP, &api_url, vault_chain, vault).await?;
        if !takeover.still_silent(latest, unix_now()) {
            tracing::info!(%vault, key = %key.address, from = %takeover.from, "the key taken over from has reported again; leaving it the vault");
            return Ok(Stop::Resumed);
        }
    }
    match approve_if_behind(
        &chain,
        &deployments,
        &approving,
        station,
        vault,
        bundle,
        taking_over(),
    )
    .await
    {
        Ok(Approval::Follows | Approval::Behind) => approvals.settled(),
        Ok(Approval::Replaced(by)) if taking_over() == Some(by) => {
            approvals.settled();
            tracing::info!(%vault, key = %key.address, from = %by, "took the vault over");
        }
        Ok(Approval::Replaced(by)) => {
            tracing::info!(%vault, key = %key.address, %by, "another executor's key holds the approval; not serving");
            return Ok(Stop::Replaced);
        }
        Err(error) => {
            if !chain::unanswered(&error)
                && refused(
                    &chain,
                    &deployments,
                    &attestation,
                    station,
                    key.address,
                    vault,
                    bundle,
                )
                .await?
            {
                tracing::warn!(%vault, key = %key.address, "the station refuses to approve the key; not serving");
                return Ok(Stop::Replaced);
            }
            return Err(error.context("the station would not approve the key"));
        }
    }
    // The approval is this key's now: a key that takes it from here on has
    // replaced this executor, which lets the vault go. Its reports may go
    // from here on.
    if let Ok(mut from) = takes_over.lock() {
        *from = None;
    }
    holds.take();

    // Keep the venue's approval following the blessing, and note whether the
    // key wants blessing again, so the instance asks for a fresh token in
    // time: a key that stopped attesting stops being able to trade. The
    // approval is looked at after every renewal and once a minute besides,
    // so one missed for any reason is caught up; both stop with the serve,
    // which ends once another executor's key is found to hold the approval.
    let renewed = Arc::new(tokio::sync::Notify::new());
    let replaced = Arc::new(AtomicBool::new(false));
    let _following = AbortOnDrop(tokio::spawn(follow_blessing(
        chain.clone(),
        deployments.clone(),
        attestation.clone(),
        approving,
        station,
        vault,
        bundle,
        approvals,
        renewed.clone(),
        replaced.clone(),
    )));
    let _tracking = AbortOnDrop(tokio::spawn(track_blessing(
        chain.clone(),
        deployments.clone(),
        attestation.clone(),
        key.address,
        vault,
        bundle,
        move || renewed.notify_one(),
    )));

    // The executor reads the manifest the builder recorded, and refuses one
    // naming a driver this image doesn't carry.
    let strategy_id = chain.read(vault, IVault::strategyIdCall {}).await?;
    let registered = chain
        .read(
            deployments.strategies,
            IStrategyRegistry::strategyOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let recorded = recorded_for(&store, &registered, bundle, &network)?;
    // Which half serves a driver is told by its name in the recorded
    // manifest: a manifest may only name drivers on the published list,
    // which is compiled into this image and fixes what each name is, and the
    // station pins its drivers under the manifest's names. Each half then
    // works under the name it was pinned under, and refuses to start unless
    // the station lists that name and pinned a driver under it.
    //
    // A Hyperliquid vault's account is traded by this vault's own key as the
    // station's agent; its events go to the vault's feed for as long as it
    // is served.
    let names = |name: &str| recorded.drivers.iter().any(|driver| driver == name);
    // It also keeps a little HYPE on HyperCore for the account's sends home,
    // buying some on the pinned HYPE/USDC pair when it runs low, and keeps
    // looking for which markets the driver lists the venue doesn't list for
    // trading.
    let (hyperliquid, _hyperliquid_events, _hyperliquid_hype, _hyperliquid_missing) =
        if names(hyperliquid::DRIVER) {
            let half = hyperliquid::Half::connect(
                &chain,
                vault,
                station,
                deployments.usd,
                vault_key.signer(),
                hyperliquid::DRIVER,
            )
            .await?;
            let events = AbortOnDrop(half.forward_events(account_events.sender()));
            let hype = AbortOnDrop(half.hype_loop());
            let missing = AbortOnDrop(half.missing_loop());
            (Some(half), Some(events), Some(hype), Some(missing))
        } else {
            (None, None, None, None)
        };
    // A Robinhood Chain vault's Safe is traded by this vault's key through
    // the Safe's role; its Lighter account by a Lighter key made now, which
    // the station registers, and whose accounts this executor posts on the
    // board for as long as it serves the vault.
    let stocks = if names(robinhood::ROBINHOOD) {
        Some(
            robinhood::StockHalf::connect(&chain, key.clone(), station, robinhood::ROBINHOOD)
                .await?,
        )
    } else {
        None
    };
    let (lighter, _lighter_posts) = if names(robinhood::LIGHTER) {
        let half =
            robinhood::LighterHalf::connect(&chain, key.clone(), station, robinhood::LIGHTER)
                .await?;
        let posts = AbortOnDrop(half.report_loop());
        (Some(half), Some(posts))
    } else {
        (None, None)
    };

    // Until the vault's halt is known (see [`Shared::halt_known`]), a halted
    // vault starts a faulting agent again at once, however long ago it
    // halted, so an agent that faulted before it heard of the halt is not
    // held off from its grace.

    // An agent that stops is started again. It rebuilds its own picture from
    // the venue, so a restart costs it nothing but the time.
    loop {
        if replaced.load(Ordering::SeqCst) {
            return Ok(Stop::Replaced);
        }
        // An agent that has wound down in the vault's halt has done its
        // part: the grace is waited out rather than the agent started again.
        if wound_down.load(Ordering::SeqCst) {
            while !finished(&chain, vault).await? {
                if replaced.load(Ordering::SeqCst) {
                    return Ok(Stop::Replaced);
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            tracing::info!(%vault, "the vault has stopped; letting it go");
            return posted_until_closed(&chain, vault, &replaced, _lighter_posts.is_some()).await;
        }
        // A dry tank, or one not yet read, holds the agent back: a tank
        // already dry when the vault was taken on is never traded on.
        if dry.load(Ordering::SeqCst) || !metered.load(Ordering::SeqCst) {
            if finished(&chain, vault).await? {
                return posted_until_closed(&chain, vault, &replaced, _lighter_posts.is_some())
                    .await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        // The author's allowances as they stand, each held to this
        // machine's ceiling. A change to what the host holds the agent to
        // starts it again under the new ones, so it applies at once.
        let allowances = ceilings.hold(&usage::shared_allowances(
            &chain.read(vault, IVault::allowancesCall {}).await?,
        ));
        tell_if_halted(&chain, vault, &vault_events).await?;
        // Whether this agent is handed the halting notice, from the feed.
        let halting_heard = Arc::new(AtomicBool::new(false));
        // Stopped with this serve however it ends, so a vault let go while
        // its agent runs leaves no watcher reading the chain for nobody.
        // The markets this start of the agent follows, as it asks about
        // them and trades them; nothing from the stand-in venue at all for
        // an agent that doesn't trade there.
        let following = watch::Following::default();
        let local_venue = recorded
            .drivers
            .iter()
            .any(|driver| driver == host::LOCAL)
            .then_some(deployments.venue);
        let watching = AbortOnDrop(tokio::spawn(watch::run(
            chain.clone(),
            vault,
            station,
            local_venue,
            following.clone(),
            watched_to.clone(),
            account_events.sender(),
            vault_events.sender(),
        )));

        // When this start began, which a halt's restart floor counts from.
        let started = tokio::time::Instant::now();
        let instantiated = Arc::new(std::sync::OnceLock::new());
        let runner = runner::Runner {
            place: host::Place::Evm(host::Evm {
                chain: chain.clone(),
                key: key.clone(),
                vault,
                station,
                venue: deployments.venue,
                usd: deployments.usd,
                factory: deployments.factory,
                hyperliquid: hyperliquid.clone(),
                stocks: stocks.clone(),
                lighter: lighter.clone(),
            }),
            home_chain: vault_chain.name().to_string(),
            drivers: recorded.drivers.clone(),
            models: Some(
                store
                    .join(format!("sha256-{}", alloy::hex::encode(bundle)))
                    .join("models"),
            ),
            network: recorded.network,
            allowances,
            halting_heard: halting_heard.clone(),
            running: running.clone(),
            instantiate_within: runner::INSTANTIATE_WITHIN,
            instantiated: instantiated.clone(),
            following,
        };
        // Read and checked at every start, before anything is billed: a
        // component swapped while the agent ran is caught when it is next
        // started, and never run.
        let component = match component_of(&store, bundle, &recorded) {
            Ok(component) => {
                // Refusals are said for as long as the latest start was
                // refused: a store put right reads as right again.
                if let Ok(mut counters) = counters.lock() {
                    counters.not_the_bundle = 0;
                }
                component
            }
            Err(error) => {
                if error.is::<NotTheBundle>() {
                    if let Ok(mut counters) = counters.lock() {
                        counters.not_the_bundle += 1;
                    }
                    tracing::error!(%vault, %bundle, error = format!("{error:#}"), "the stored component is not the vault's bundle; not running it");
                }
                return Err(error);
            }
        };
        let mut changed = false;
        let mut ended_cleanly = false;
        // Billed as running from when its instantiation starts until the
        // select ends, however it ends: the runner holds the flag.
        let stopped_dry = tokio::select! {
            outcome = runner.run(
                &engine,
                &component,
                account_events.reader(),
                vault_events.reader(),
                counters.clone(),
            ) => {
                match outcome {
                    Ok(stopped) => ended_cleanly = stopped == runner::Stopped::Finished,
                    // Already cut to what the log takes of a strategy's words.
                    Err(error) => tracing::warn!(%vault, error = format!("{error:#}"), "the agent could not be run"),
                }
                false
            }
            // A dry tank stops the agent where it is, positions left as they
            // are, until the author tops it up.
            _ = until(dry.clone()) => true,
            _ = until_changed(&chain, vault, allowances, ceilings) => {
                changed = true;
                false
            }
            // Once the grace is over the station has stopped, and the agent
            // is let go wherever it is.
            _ = until_finished(&chain, vault) => false,
            // Another executor has taken the vault over: its agent trades
            // now, and this one stops where it is.
            _ = until(replaced.clone()) => false,
        };
        drop(watching);
        // Only an agent that ran for a good while, however it stopped,
        // starts the count of failed starts again. Its time is counted from
        // when it was instantiated: compiling it is the host's work, and
        // says nothing of whether the agent runs cleanly.
        if instantiated
            .get()
            .is_some_and(|at| at.elapsed() >= RAN_FOR_A_WHILE)
        {
            failed_starts.store(0, Ordering::SeqCst);
        }
        if replaced.load(Ordering::SeqCst) {
            return Ok(Stop::Replaced);
        }

        // Once the vault has halted and its grace is over, the station is
        // stopped and nothing is left to run.
        if halting_heard.load(Ordering::SeqCst) {
            halt_known.store(true, Ordering::SeqCst);
        }
        let done = done_its_part(ended_cleanly, halting_heard.load(Ordering::SeqCst));
        if after_the_agent(&chain, vault, &wound_down, done).await? {
            tracing::info!(%vault, "the vault has stopped; letting it go");
            return posted_until_closed(&chain, vault, &replaced, _lighter_posts.is_some()).await;
        }
        if stopped_dry {
            continue;
        }
        if changed {
            tracing::info!(%vault, "the allowances changed; starting the agent under the new ones");
            continue;
        }
        // An agent that has wound down waits out the grace, at the top of
        // the loop, instead of starting again.
        if done {
            continue;
        }
        if let Ok(mut counters) = counters.lock() {
            counters.restarts += 1;
        }
        // One that stopped soon after it was started, however far it got, is
        // held off for longer each time it does, as a failing serve is.
        let failed = failed_starts.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::warn!(%vault, "restarting the strategy");
        // Cut short by whatever the serve acts on while the agent runs: a
        // change to the allowances, which is started under at once; the
        // vault's halt, not yet known, which the agent is started again at
        // once to wind down in; the vault closing or its grace ending, or another
        // executor taking it over, which end the serve.
        match hold_off(
            &chain,
            vault,
            started,
            backoff(failed),
            allowances,
            ceilings,
            &replaced,
            halt_known.load(Ordering::SeqCst),
        )
        .await
        {
            None => {}
            Some(Woken::Changed) => {
                tracing::info!(%vault, "the allowances changed; starting the agent under the new ones");
            }
            Some(Woken::Halted) => {
                tracing::info!(%vault, "the vault has halted; starting the agent again to wind down");
            }
            Some(Woken::Finished) => {
                tracing::info!(%vault, "the vault has stopped; letting it go");
                return posted_until_closed(&chain, vault, &replaced, _lighter_posts.is_some())
                    .await;
            }
            Some(Woken::Replaced) => return Ok(Stop::Replaced),
        }
    }
}

/// How often Hyperliquid's spot pairs are read again for their feed names.
const SPOT_PAIRS_READ_EVERY: Duration = Duration::from_secs(600);

/// How often a halted vault whose Lighter accounts are still posted for is
/// looked at to see whether it has closed.
const POSTED_UNTIL_CLOSED_LOOK: Duration = Duration::from_secs(10);

/// The serve's end once its agent has nothing left to run. A Lighter
/// account is valued and its closes proved only by posts read after them,
/// and a halted vault's full unwind runs after the agent's grace: so while
/// this executor posts for the vault's Lighter accounts it goes on posting,
/// its post loop kept alive here, until the vault has closed (everything
/// its stations held has come home or been given up) or another executor's
/// key holds the station's approval.
async fn posted_until_closed(
    chain: &Client,
    vault: Address,
    replaced: &AtomicBool,
    posting: bool,
) -> Result<Stop> {
    if !posting {
        return Ok(Stop::Finished);
    }
    tracing::info!(%vault, "the agent's grace is over; posting the Lighter accounts until the vault closes");
    loop {
        if replaced.load(Ordering::SeqCst) {
            return Ok(Stop::Replaced);
        }
        match chain.read(vault, IVault::stateCall {}).await {
            Ok(VaultState::None | VaultState::Closed | VaultState::Cancelled) => {
                tracing::info!(%vault, "the vault has closed; letting it go");
                return Ok(Stop::Finished);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%vault, error = %format!("{error:#}"), "the vault's state wasn't read");
            }
        }
        tokio::time::sleep(POSTED_UNTIL_CLOSED_LOOK).await;
    }
}

/// What cut a faulting agent's held-off restart short.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Woken {
    /// What the host holds the agent to of the allowances changed.
    Changed,
    /// The vault halted.
    Halted,
    /// The vault has nothing left to run (see [`finished`]).
    Finished,
    /// Another executor holds the vault.
    Replaced,
}

/// What a held-off restart looks at as it waits.
struct Look {
    replaced: bool,
    finished: bool,
    halted: bool,
    /// What the host would hold the agent to now, if it could be read.
    held_to: Option<shared::Allowances>,
}

/// Whether what is seen now cuts a held-off restart short, for an agent held
/// to `running` (if it was ever read), whose vault's halt is (`halt_known`)
/// or is not yet known, `waited` since the agent was last started (or the
/// wait began). A halt not yet known cuts a wait short only once
/// [`HALT_RESTART_FLOOR`] has passed: an agent that
/// faults before it is ever told of the halt would otherwise be started
/// again at every look for the whole grace.
fn woken(
    running: Option<&shared::Allowances>,
    halt_known: bool,
    waited: Duration,
    now: &Look,
) -> Option<Woken> {
    if now.replaced {
        Some(Woken::Replaced)
    } else if now.finished {
        Some(Woken::Finished)
    } else if now.halted && !halt_known && waited >= HALT_RESTART_FLOOR {
        Some(Woken::Halted)
    } else if let (Some(running), Some(held_to)) = (running, &now.held_to)
        && restarts_under(running, held_to)
    {
        Some(Woken::Changed)
    } else {
        None
    }
}

/// Wait out a faulting agent's restart, `wait` long, unless something the
/// serve acts on happens first, and say what. A read that fails is taken as
/// nothing having changed, so the wait is never cut short by the node's
/// trouble. A halt cuts it short unless it is already known (`halt_known`),
/// whether it lands during the wait or landed before it unheard of, though
/// never before [`HALT_RESTART_FLOOR`] has passed since the agent was last
/// `started`.
#[allow(clippy::too_many_arguments)]
async fn hold_off(
    chain: &Client,
    vault: Address,
    started: tokio::time::Instant,
    wait: Duration,
    allowances: shared::Allowances,
    ceilings: ceilings::Ceilings,
    replaced: &AtomicBool,
    halt_known: bool,
) -> Option<Woken> {
    let until = tokio::time::Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        tokio::time::sleep(left.min(Duration::from_secs(2))).await;
        let now = look(chain, vault, ceilings, replaced.load(Ordering::SeqCst)).await;
        if let Some(woken) = woken(Some(&allowances), halt_known, started.elapsed(), &now) {
            return Some(woken);
        }
    }
}

/// What a held-off restart looks at, `replaced` as the caller knows it. A
/// read that fails is taken as nothing having changed.
async fn look(
    chain: &Client,
    vault: Address,
    ceilings: ceilings::Ceilings,
    replaced: bool,
) -> Look {
    Look {
        replaced,
        finished: matches!(finished(chain, vault).await, Ok(true)),
        halted: matches!(halted(chain, vault).await, Ok(true)),
        held_to: held_to(chain, vault, ceilings).await,
    }
}

/// What the host would hold the agent to of the vault's allowances now, if
/// they can be read.
async fn held_to(
    chain: &Client,
    vault: Address,
    ceilings: ceilings::Ceilings,
) -> Option<shared::Allowances> {
    chain
        .read(vault, IVault::allowancesCall {})
        .await
        .ok()
        .map(|now| ceilings.hold(&usage::shared_allowances(&now)))
}

/// A vault whose restart the sweep holds off, as the sweep already knows it.
struct HeldOff {
    vault: Address,
    /// The vault's state, as this sweep read it.
    state: VaultState,
    station: Address,
    /// The vault's key here.
    key: Address,
}

/// What a restart the sweep holds off looks at, at every sweep: only what
/// its wake needs. The state is the sweep's own read and the station is
/// known; the halt is read once, and the grace and the clock only for a
/// halted vault; another executor's key is looked into only when the
/// station approves one; and the allowances are read only when there are
/// some to compare with (`ceilings` given). A read that fails is taken as
/// nothing having changed.
async fn look_held(
    chain: &Client,
    deployments: &Deployments,
    held: HeldOff,
    ceilings: Option<ceilings::Ceilings>,
) -> Look {
    let halted_at = chain
        .read(
            held.vault,
            chain::bindings::station::IStationVault::haltedAtCall {},
        )
        .await
        .unwrap_or(0);
    let finished = matches!(held.state, VaultState::None)
        || (halted_at != 0 && grace_over(chain, held.vault, halted_at).await == Some(true));
    let approved = match key_driver(chain, held.station).await {
        Ok(driver) => chain
            .read(held.station, IStation::approvedKeyCall { driver })
            .await
            .unwrap_or(Address::ZERO),
        Err(_) => Address::ZERO,
    };
    let replaced = approved != Address::ZERO
        && approved != held.key
        && matches!(
            approval_now(chain, deployments, held.key, held.vault).await,
            Ok(Approval::Replaced(_))
        );
    let held_to = match ceilings {
        Some(ceilings) => held_to(chain, held.vault, ceilings).await,
        None => None,
    };
    Look {
        replaced,
        finished,
        halted: halted_at != 0,
        held_to,
    }
}

/// Whether the grace of a vault halted at `halted_at` is over, if it can be
/// read.
async fn grace_over(chain: &Client, vault: Address, halted_at: u64) -> Option<bool> {
    let grace = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltGraceCall {},
        )
        .await
        .ok()?;
    Some(chain.block_timestamp().await.ok()? >= halted_at + grace)
}

/// Whether the vault has halted.
async fn halted(chain: &Client, vault: Address) -> Result<bool> {
    Ok(chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltedAtCall {},
        )
        .await?
        != 0)
}

/// How long an agent must run, from when it is instantiated, before it stops
/// for its restart to be held off no longer than the first time: one that
/// stops sooner, whether in its start functions or in `run`, is held off for
/// longer each time.
const RAN_FOR_A_WHILE: Duration = Duration::from_secs(60);

/// Whether an agent that stopped has done its part in the vault's halt: it
/// ended without a fault, after it was handed the halting notice.
fn done_its_part(ended_cleanly: bool, told_of_the_halt: bool) -> bool {
    ended_cleanly && told_of_the_halt
}

/// What follows an agent's stop: whether the vault has nothing left to run.
///
/// An agent that ended without a fault after it was handed the halting
/// notice (`done`) has done its part, and waits out the grace instead of
/// starting again. That is recorded before anything is read: a read that
/// fails here ends the serve, and the serve that follows must still find the
/// agent wound down rather than start it again. One that faulted, or could
/// not be started, or ended before it was told, is started again while the
/// grace lasts, so it can still wind down; a halt is the time it matters
/// most.
async fn after_the_agent(
    chain: &Client,
    vault: Address,
    wound_down: &AtomicBool,
    done: bool,
) -> Result<bool> {
    if done && !wound_down.swap(true, Ordering::SeqCst) {
        tracing::info!(%vault, "the agent has wound down while the vault halts");
    }
    finished(chain, vault).await
}

/// Before an agent starts, put the halting notice first in its vault's feed
/// if the vault has already halted. The watcher only tells of a halt it
/// reads, and one started after the halt never reads it, so an agent started
/// in the grace (the executor restarted, the agent started again) would
/// otherwise trade on, untold, until its positions are closed under it.
async fn tell_if_halted(
    chain: &Client,
    vault: Address,
    vault_events: &feed::Feed<watch::VaultEvent>,
) -> Result<()> {
    if let Some(notice) = watch::halting_notice(chain, vault).await? {
        vault_events.put_first(notice, |event| {
            matches!(event, watch::VaultEvent::Halting(_))
        });
    }
    Ok(())
}

/// Whether the registry counts `key` as this vault's executor for `bundle`,
/// when its blessing ends, and the chain's blessing TTL and clock.
async fn blessing_of(
    chain: &Client,
    deployments: &Deployments,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<(bool, u64, u64, u64)> {
    let vault_chain = alloy::primitives::U256::from(deployments.chain_id);
    let blessed = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::isBlessedForCall {
                signer: key,
                vaultChain: vault_chain,
                vault,
                strategyHash: bundle,
            },
        )
        .await?;
    let until = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessedUntilCall {
                signer: key,
                vaultChain: vault_chain,
                vault,
                strategyHash: bundle,
            },
        )
        .await?;
    let ttl = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessingTtlCall {},
        )
        .await?;
    let now = chain.block_timestamp().await?;
    Ok((blessed, until, ttl, now))
}

/// Wait until the keeper has blessed the key for this vault and bundle and
/// stocked it with gas, noting in the instance's tree whether it still
/// wants blessing. What is awaited is said once, not at every look.
async fn wait_blessed_and_stocked(
    chain: &Client,
    deployments: &Deployments,
    attestation: &attestation::Attestation,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<()> {
    let mut said = None;
    loop {
        let (blessed, until, ttl, now) =
            blessing_of(chain, deployments, key, vault, bundle).await?;
        attestation.note(
            deployments.chain_id,
            vault,
            key,
            attestation::due(blessed, until, now, ttl),
            until,
            ttl,
        );
        let waiting_for = if blessed {
            let line = health::gas_line(chain).await?;
            if chain.balance(key).await? >= line {
                tracing::info!(%vault, %key, "the key is blessed and holds its gas");
                return Ok(());
            }
            "its gas"
        } else {
            "its blessing"
        };
        if said != Some(waiting_for) {
            tracing::info!(%vault, %key, "the key waits for {waiting_for} (the keeper's)");
            said = Some(waiting_for);
        }
        tokio::time::sleep(GAS_LOOK).await;
    }
}

/// For as long as the serve runs, note whether the key wants blessing again
/// (a third of its blessing left, or none), so the instance attests afresh
/// in time for the keeper to renew it; and run `on_renewal` whenever its
/// blessing is seen to move, so the venue's approval follows it. A read that
/// fails is looked at again shortly.
async fn track_blessing(
    chain: Client,
    deployments: Deployments,
    attestation: attestation::Attestation,
    key: Address,
    vault: Address,
    bundle: B256,
    mut on_renewal: impl FnMut() + Send + 'static,
) {
    let mut seen_until = None;
    let mut unread = Throttle::default();
    loop {
        match blessing_of(&chain, &deployments, key, vault, bundle).await {
            Ok((blessed, until, ttl, now)) => {
                attestation.note(
                    deployments.chain_id,
                    vault,
                    key,
                    attestation::due(blessed, until, now, ttl),
                    until,
                    ttl,
                );
                if seen_until.is_some_and(|seen| seen != until) {
                    on_renewal();
                }
                seen_until = Some(until);
            }
            Err(error) => {
                if unread.due(Instant::now()) {
                    tracing::warn!(%vault, %key, error = format!("{error:#}"), "could not tell how long the key's blessing has left; looking again shortly");
                }
            }
        }
        tokio::time::sleep(BLESSING_LOOK).await;
    }
}

/// How often a served key's blessing is looked at.
const BLESSING_LOOK: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(15)
};

/// Where the station's approval stands for this key.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Approval {
    /// This key is approved at the venue until its blessing ends: approving
    /// it again would change nothing.
    Follows,
    /// Approving this key is what keeps it trading: it is approved with a
    /// venue expiry behind its blessing, or no key is approved, or the key
    /// approved is another whose blessing has lapsed.
    Behind,
    /// Another key, still blessed as this vault's executor, holds the
    /// approval: another executor has taken the vault over. One executor at
    /// a time, so this one doesn't take it back.
    Replaced(Address),
}

/// Where the station's approval stands for this key, the vault's station and
/// bundle read now.
async fn approval_now(
    chain: &Client,
    deployments: &Deployments,
    key: Address,
    vault: Address,
) -> Result<Approval> {
    let station = chain.read(vault, IVault::homeStationCall {}).await?;
    let bundle = chain.read(vault, IVault::bundleHashCall {}).await?;
    approval_of(chain, deployments, station, key, vault, bundle).await
}

/// Where the station's approval stands for this key (see [`Approval`]).
async fn approval_of(
    chain: &Client,
    deployments: &Deployments,
    station: Address,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<Approval> {
    let driver = key_driver(chain, station).await?;
    let approved = chain
        .read(station, IStation::approvedKeyCall { driver })
        .await?;
    let vault_chain = alloy::primitives::U256::from(deployments.chain_id);
    if approved == Address::ZERO {
        return Ok(Approval::Behind);
    }
    if approved != key {
        // As the station judges it: a key blessed for this vault and bundle,
        // as an executor.
        let blessed = chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer: approved,
                    vaultChain: vault_chain,
                    vault,
                    strategyHash: bundle,
                },
            )
            .await?
            && matches!(
                chain
                    .read(
                        deployments.attestation,
                        IAttestationRegistry::keyInfoCall { signer: approved },
                    )
                    .await?
                    .kind,
                chain::bindings::attestation::ImageKind::Executor
            );
        return Ok(if blessed {
            Approval::Replaced(approved)
        } else {
            Approval::Behind
        });
    }
    let blessed_until = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::blessedUntilCall {
                signer: key,
                vaultChain: vault_chain,
                vault,
                strategyHash: bundle,
            },
        )
        .await?;
    // Where the venue keeps its own record of the approval, it is read
    // there: an approval the venue dropped, or one that ends before the
    // blessing, is behind. A venue whose approval has no end of its own
    // follows the station's.
    let venue_until = if driver == chain::names::LOCAL_DRIVER {
        chain
            .read(
                deployments.venue,
                ILocalVenue::agentValidUntilCall {
                    master: station,
                    agent: key,
                },
            )
            .await?
    } else if driver == hyperliquid::driver_name() {
        hyperliquid::agent_valid_until(chain.chain_id, station, key).await?
    } else {
        return Ok(Approval::Follows);
    };
    Ok(if venue_until < blessed_until {
        Approval::Behind
    } else {
        Approval::Follows
    })
}

/// The driver whose approved key at the station is the vault's own executor
/// key: what says which executor holds the vault. The first of the
/// station's drivers that approves that key, in the order the station lists
/// them; a driver that registers a key of its own at the venue (Lighter's,
/// made at every start) is never it. A station whose drivers are all of
/// that kind falls back to the first.
///
/// A station's drivers never change, so each station's is read once. One
/// whose list can't be read yet is taken as the stand-in's, as before
/// stations had other drivers: for a station that doesn't declare it, the
/// approval reads as behind and its approval is refused by the station,
/// which costs a try and is made again on the next sweep.
async fn key_driver(chain: &Client, station: Address) -> Result<B256> {
    static KNOWN: std::sync::LazyLock<std::sync::Mutex<HashMap<(u64, Address), B256>>> =
        std::sync::LazyLock::new(Default::default);
    let at = (chain.chain_id, station);
    if let Some(driver) = KNOWN.lock().ok().and_then(|known| known.get(&at).copied()) {
        return Ok(driver);
    }
    match chain.read(station, IStation::specCall {}).await {
        Ok(spec) => {
            let driver = key_driver_of(&spec.drivers).context("the station lists no driver")?;
            if let Ok(mut known) = KNOWN.lock() {
                known.insert(at, driver);
            }
            Ok(driver)
        }
        Err(error) => {
            tracing::debug!(%station, error = format!("{error:#}"), "the station's drivers were not read; taking the stand-in's");
            Ok(chain::names::LOCAL_DRIVER)
        }
    }
}

/// [`key_driver`], from the station's list of drivers.
fn key_driver_of(drivers: &[B256]) -> Option<B256> {
    let own_key = chain::names::name(robinhood::LIGHTER.as_bytes());
    drivers
        .iter()
        .find(|driver| **driver != own_key)
        .or_else(|| drivers.first())
        .copied()
}

/// Have the station approve the key at the venue, until its blessing ends,
/// if where the approval stands, read once the key's turn to send has come,
/// is still behind; and say where it stood then. Read before the turn, it
/// could be stale by the time the approval is sent: one of the agent's
/// orders may be what holds the turn, and another executor may take the
/// vault over meanwhile, which an approval sent on the stale look would take
/// back (`Station.approveKey` lets a blessed key replace another). Only the
/// key named in `takes_over`, whose executor has gone silent, is ever
/// replaced on purpose.
async fn approve_if_behind(
    chain: &Client,
    deployments: &Deployments,
    key: &chain::KeyClient,
    station: Address,
    vault: Address,
    bundle: B256,
    takes_over: Option<Address>,
) -> Result<Approval> {
    let driver = key_driver(chain, station).await?;
    let mut found = None;
    key.send_if(
        async {
            let approval =
                approval_of(chain, deployments, station, key.address, vault, bundle).await?;
            found = Some(approval);
            Ok(match approval {
                Approval::Behind => true,
                Approval::Replaced(by) => takes_over == Some(by),
                Approval::Follows => false,
            })
        },
        station,
        IStation::approveKeyCall {
            driver,
            key: key.address,
        },
    )
    .await?;
    found.context("the approval was never read")
}

/// Whether the station would refuse to approve this key, however often it is
/// asked: the driver it approves keys for is revoked (or not there), or the
/// key isn't blessed as this vault's executor and can't be blessed again, its
/// image or project no longer allowed. A key whose blessing has only lapsed
/// is blessed again, and approved after. The image and project are the ones
/// the instance attests as; while it doesn't know them yet (no token yet),
/// they are not held against it.
async fn refused(
    chain: &Client,
    deployments: &Deployments,
    attestation: &attestation::Attestation,
    station: Address,
    key: Address,
    vault: Address,
    bundle: B256,
) -> Result<bool> {
    let driver = key_driver(chain, station).await?;
    let pinned = chain
        .read(station, IStation::pinnedDriverCall { driver })
        .await?;
    if pinned == Address::ZERO
        || chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isDriverRevokedCall {
                    name: driver,
                    implementation: pinned,
                },
            )
            .await?
    {
        return Ok(true);
    }
    let blessed = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::isBlessedForCall {
                signer: key,
                vaultChain: alloy::primitives::U256::from(deployments.chain_id),
                vault,
                strategyHash: bundle,
            },
        )
        .await?;
    if blessed {
        return Ok(false);
    }
    let Some((image_digest, project_id)) = attestation.identity() else {
        return Ok(false);
    };
    let image = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::isImageAllowedCall {
                imageDigest: image_digest,
            },
        )
        .await?;
    let project = chain
        .read(
            deployments.attestation,
            IAttestationRegistry::isProjectAllowedCall {
                projectId: project_id,
            },
        )
        .await?;
    Ok(!(image && project))
}

/// How long to wait before an approval that failed is tried again.
fn approval_retry(failures: u32) -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        backoff(failures)
    }
}

/// How following the approval ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    /// The approval is this key's, following its blessing.
    Kept,
    /// Another executor's key, still blessed, holds it.
    Replaced,
    /// The station refuses to approve this key (see [`refused`]).
    Refused,
}

/// Approve the key at the venue after a renewal, trying again ever more
/// slowly until it lands: until it does, the venue turns the agent's orders
/// away from the old blessing's end. Where the approval stands is read in the
/// key's turn to send, immediately before every send, the first included,
/// and the key approved only while that still reads behind: a look taken
/// earlier may be stale (a send that failed may have landed all the same, or
/// another executor may have taken the vault over meanwhile), and approving
/// on it would take the vault back from a successor. An approval that went
/// unanswered once broadcast may still land, for as long as the chain client
/// watches for its receipt: while it may, no other is sent, and where the approval
/// stands is only looked at. One that failed before it was broadcast (its
/// estimate, say) can't land, and is tried again after the usual wait. One
/// the station refuses ends the follow. A read that fails is tried again
/// later.
#[allow(clippy::too_many_arguments)]
async fn approve_until_it_lands(
    chain: &Client,
    deployments: &Deployments,
    attestation: &attestation::Attestation,
    key: &chain::KeyClient,
    station: Address,
    vault: Address,
    bundle: B256,
    unanswered: &chain::Unanswered,
) -> Held {
    // Each approval sent here is noted in `unanswered` by its send, the
    // moment it is broadcast.
    let key = &key.noting(unanswered);
    let mut failures = 0;
    let mut said = Throttle::default();
    loop {
        let tried = match approval_of(chain, deployments, station, key.address, vault, bundle).await
        {
            Ok(Approval::Follows) => {
                unanswered.settled();
                return Held::Kept;
            }
            Ok(Approval::Replaced(_)) => return Held::Replaced,
            Ok(Approval::Behind) if unanswered.may_still_land(key.address) => {
                tokio::time::sleep(approval_retry(1)).await;
                continue;
            }
            Ok(Approval::Behind) => {
                match approve_if_behind(chain, deployments, key, station, vault, bundle, None).await
                {
                    Ok(Approval::Follows | Approval::Behind) => {
                        unanswered.settled();
                        return Held::Kept;
                    }
                    Ok(Approval::Replaced(_)) => return Held::Replaced,
                    // Noted by the send itself when it was broadcast.
                    Err(error) if chain::may_still_land(&error) => error,
                    Err(error) => {
                        if refused(
                            chain,
                            deployments,
                            attestation,
                            station,
                            key.address,
                            vault,
                            bundle,
                        )
                        .await
                        .unwrap_or(false)
                        {
                            return Held::Refused;
                        }
                        error
                    }
                }
            }
            Err(error) => error.context("reading where the approval stands"),
        };
        failures += 1;
        if said.due(Instant::now()) {
            tracing::warn!(%vault, error = format!("{tried:#}"), "the venue's approval did not follow; trying again");
        }
        tokio::time::sleep(approval_retry(failures)).await;
    }
}

/// How often a serve held back by a send that may still land looks again.
const LANDING_LOOK: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(5)
};

/// How often the venue's approval is checked against the blessing while a
/// serve runs.
const FOLLOW_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(60)
};

/// Keep the venue's approval of the key following its blessing for as long
/// as the serve runs: whenever the approval is behind (see [`Approval`]),
/// approve the key again until that lands. Looked at once every
/// [`FOLLOW_EVERY`], and at once after a renewal. A look that can't be read
/// is left for the next. Once another executor's key, still blessed, holds
/// the approval, this one stops following and raises `replaced`, which ends
/// the serve: one executor at a time, and the two would otherwise take the
/// vault from each other at every look. So does a station that refuses to
/// approve this key: its agent can't trade, and isn't billed for running.
#[allow(clippy::too_many_arguments)]
async fn follow_blessing(
    chain: Client,
    deployments: Deployments,
    attestation: attestation::Attestation,
    key: chain::KeyClient,
    station: Address,
    vault: Address,
    bundle: B256,
    approvals: chain::Unanswered,
    renewed: Arc<tokio::sync::Notify>,
    replaced: Arc<AtomicBool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(FOLLOW_EVERY) => {}
            _ = renewed.notified() => {}
        }
        let held =
            match approval_of(&chain, &deployments, station, key.address, vault, bundle).await {
                Ok(Approval::Behind) => {
                    approve_until_it_lands(
                        &chain,
                        &deployments,
                        &attestation,
                        &key,
                        station,
                        vault,
                        bundle,
                        &approvals,
                    )
                    .await
                }
                Ok(Approval::Replaced(_)) => Held::Replaced,
                Ok(Approval::Follows) | Err(_) => Held::Kept,
            };
        match held {
            Held::Kept => continue,
            Held::Replaced => {
                tracing::info!(%vault, key = %key.address, "another executor's key holds the approval; no longer following it");
            }
            Held::Refused => {
                tracing::warn!(%vault, key = %key.address, "the station refuses to approve the key; stopping its agent");
            }
        }
        replaced.store(true, Ordering::SeqCst);
        return;
    }
}

/// Stops the task it holds when it is dropped: when the vault it serves is
/// let go, or the serve that started it ends, however it ends.
struct AbortOnDrop<T = ()>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Awaited as the task itself is, and stopped if dropped before it ends.
impl<T> std::future::Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// Resolve when the flag is set.
async fn until(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Whether an agent running under `running` is to be started again under
/// `now`: only when what the host holds it to has changed. The meter bills
/// the allowances it reads at once, whether or not the agent is started
/// again, so a change to gas, requests, bytes, streams or inference alone
/// leaves the agent running.
fn restarts_under(running: &shared::Allowances, now: &shared::Allowances) -> bool {
    runner::enforced(running) != runner::enforced(now)
}

/// Resolve once what the host holds the agent to of the vault's allowances,
/// held to the ceilings, is no longer what it holds it to under these.
async fn until_changed(
    chain: &Client,
    vault: Address,
    allowances: shared::Allowances,
    ceilings: ceilings::Ceilings,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Ok(now) = chain.read(vault, IVault::allowancesCall {}).await
            && restarts_under(&allowances, &ceilings.hold(&usage::shared_allowances(&now)))
        {
            return;
        }
    }
}

/// Resolve once the vault has nothing left to run.
async fn until_finished(chain: &Client, vault: Address) {
    loop {
        if matches!(finished(chain, vault).await, Ok(true)) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Whether there is nothing left to run: the vault never opened or has
/// closed, or it halted and the agent's grace is over.
async fn finished(chain: &Client, vault: Address) -> Result<bool> {
    let state = chain.read(vault, IVault::stateCall {}).await?;
    if matches!(
        state,
        VaultState::None | VaultState::Closed | VaultState::Cancelled
    ) {
        return Ok(true);
    }
    let halted = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltedAtCall {},
        )
        .await?;
    if halted == 0 {
        return Ok(false);
    }
    let grace = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltGraceCall {},
        )
        .await?;
    Ok(chain.block_timestamp().await? >= halted + grace)
}

/// What the builder recorded for a vault's bundle, as far as running it goes.
struct Recorded {
    /// The drivers the agent trades through.
    drivers: Vec<String>,
    /// Whether the manifest declares `network`.
    network: bool,
    /// The hash of the component the bundle was built to: the only bytes
    /// that may be run for it.
    component_hash: String,
}

/// What the registry recorded for the vault's bundle, from the manifest the
/// builder recorded. A manifest that isn't the registry's, or that was
/// recorded for another bundle, is refused; so is one naming a driver this
/// image doesn't carry, or one not offered on this network.
fn recorded_for(
    store: &std::path::Path,
    registered: &chain::bindings::strategies::Strategy,
    bundle: B256,
    network: &str,
) -> Result<Recorded> {
    recorded_in(
        store,
        registered.bundleHash,
        registered.manifestHash,
        bundle,
        network,
    )
}

/// The same, from the bundle and manifest hashes the registry recorded,
/// whatever chain recorded them.
fn recorded_in(
    store: &std::path::Path,
    registered_bundle: B256,
    manifest_hash: B256,
    bundle: B256,
    network: &str,
) -> Result<Recorded> {
    anyhow::ensure!(
        registered_bundle == bundle,
        "the registry records bundle {registered_bundle} for this strategy, and the vault runs {bundle}"
    );
    let hash = format!("sha256:{}", alloy::hex::encode(manifest_hash));
    let path = store.join(hash.replace(':', "-")).join("manifest.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let canonical: shared::Canonical =
        serde_json::from_slice(&bytes).context("the recorded manifest")?;
    // The store is only believed where its bytes match what the chain holds.
    anyhow::ensure!(
        canonical.hash() == hash,
        "the stored manifest is not the one the registry recorded"
    );
    anyhow::ensure!(
        canonical.bundle_hash == format!("sha256:{}", alloy::hex::encode(bundle)),
        "the recorded manifest is for bundle {}, and the vault runs {bundle}",
        canonical.bundle_hash
    );
    let catalogue = shared::Catalogue::published();
    canonical
        .manifest
        .check(&catalogue, network)
        .map_err(|why| anyhow::anyhow!("this image won't run it: {why}"))?;
    // This image carries four drivers' executor halves: the stand-in
    // venue's, Hyperliquid's, and Robinhood Chain's two (its stock tokens
    // and Lighter), which an agent may use together.
    let drivers: Vec<String> = canonical.manifest.drivers.keys().cloned().collect();
    let robinhood_chain = |name: &str| name == robinhood::ROBINHOOD || name == robinhood::LIGHTER;
    let carried = match drivers.as_slice() {
        [only] if only.as_str() == host::LOCAL || only.as_str() == hyperliquid::DRIVER => true,
        some => !some.is_empty() && some.iter().all(|name| robinhood_chain(name)),
    };
    anyhow::ensure!(
        carried,
        "this image carries the local or hyperliquid driver alone, or robinhood and lighter together, and the manifest names {drivers:?}"
    );
    Ok(Recorded {
        drivers,
        network: canonical
            .manifest
            .capabilities
            .iter()
            .any(|declared| declared == NETWORK),
        component_hash: canonical.component_hash,
    })
}

/// A component in the store whose bytes are not the ones the chain binds.
#[derive(Debug)]
struct NotTheBundle {
    found: String,
    recorded: String,
}

impl std::fmt::Display for NotTheBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the stored component hashes to {}, and the bundle's manifest records {}",
            self.found, self.recorded
        )
    }
}

impl std::error::Error for NotTheBundle {}

/// The bundle's component, read once and checked against the hash its
/// manifest records: the bytes returned are the ones that are run, so what
/// is checked is what is run. The store sits on the host's disk, outside
/// anything attested; a blessing binds the bundle's hash, and holds only if
/// the image runs the code that hash names.
fn component_of(store: &std::path::Path, bundle: B256, recorded: &Recorded) -> Result<Vec<u8>> {
    let path = bundle_path(store, bundle);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let found = shared::manifest::hash_bytes(&bytes);
    if found != recorded.component_hash {
        return Err(NotTheBundle {
            found,
            recorded: recorded.component_hash.clone(),
        }
        .into());
    }
    Ok(bytes)
}

/// Where a bundle's component lives once the build service has written it.
fn bundle_path(store: &std::path::Path, bundle: B256) -> PathBuf {
    store
        .join(format!("sha256-{}", alloy::hex::encode(bundle)))
        .join("component.wasm")
}

/// The status port. The keeper and the author's withdraw screen get a
/// vault's signed total only there, so one that can't be bound stops the
/// executor, rather than running on with every total uncollectable, or
/// served by whatever else holds the port.
async fn bind_status(address: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding the status port at {address}"))
}

/// The machine's own health (unavailable while a vault chain it serves isn't
/// connected), each vault's signed running total for the keeper to collect,
/// the instance's attestation (see [`attestation::routes`]), and the root
/// key service where this executor holds the root (see [`root`]).
async fn serve_status(
    listener: tokio::net::TcpListener,
    totals: usage::Totals,
    solana_totals: solana::Totals,
    chains: health::Chains,
    attestation: attestation::Attestation,
    root: Option<shonin_rootkey::Service<shonin_rootkey::NetworkView>>,
) {
    let mut app = attestation::routes(attestation);
    if let Some(service) = root {
        app = app.merge(shonin_rootkey::routes(service));
    }
    let app = app
        .route(
            "/health",
            axum::routing::get(move || {
                let chains = chains.clone();
                async move { chains.answer() }
            }),
        )
        .route(
            "/usage/{chain}/{vault}",
            // An EVM chain by its id and a vault by its 0x address; a Solana
            // cluster by its name and a vault by its base58 address.
            axum::routing::get(
                move |axum::extract::Path((chain, vault)): axum::extract::Path<(
                    String,
                    String,
                )>| {
                    let totals = totals.clone();
                    let solana_totals = solana_totals.clone();
                    async move {
                        let found = match (chain.parse::<u64>(), vault.parse::<Address>()) {
                            (Ok(chain), Ok(vault)) => totals
                                .lock()
                                .ok()
                                .and_then(|totals| totals.get(&(chain, vault)).cloned())
                                .map(|signed| axum::Json(signed).into_response()),
                            (Ok(_), Err(_)) => {
                                return axum::http::StatusCode::BAD_REQUEST.into_response();
                            }
                            (Err(_), _) => solana_totals
                                .lock()
                                .ok()
                                .and_then(|totals| totals.get(&(chain, vault)).cloned())
                                .map(|signed| axum::Json(signed).into_response()),
                        };
                        found.unwrap_or_else(|| axum::http::StatusCode::NOT_FOUND.into_response())
                    }
                },
            ),
        );
    // A server that stops is said out loud rather than leaving every total
    // uncollectable unexplained.
    if let Err(error) = axum::serve(listener, app).await {
        tracing::error!(%error, "the status server stopped; no signed totals will be served");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_serve_is_retried_ever_more_slowly_up_to_five_minutes() {
        let waits: Vec<u64> = (1..=9).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(backoff(u32::MAX), Duration::from_secs(300));
    }

    /// A faulting agent's held-off restart is cut short by whatever the
    /// serve acts on: another executor holding the vault, the vault having
    /// nothing left to run, the vault halting, or a change to what the host
    /// holds the agent to. A halt already known, a change the host does not
    /// hold the agent to, and a read that failed cut it short by nothing.
    #[test]
    fn a_held_off_restart_wakes_on_whatever_the_serve_acts_on() {
        let running = shared::Allowances::modest();
        let quiet = || Look {
            replaced: false,
            finished: false,
            halted: false,
            held_to: Some(running),
        };
        assert_eq!(
            woken(Some(&running), false, HALT_RESTART_FLOOR, &quiet()),
            None
        );
        assert_eq!(
            woken(
                Some(&running),
                false,
                HALT_RESTART_FLOOR,
                &Look {
                    held_to: None,
                    ..quiet()
                }
            ),
            None,
            "a read that failed woke the wait"
        );
        let billed_only = shared::Allowances {
            gas_per_day: running.gas_per_day * 2.0,
            ..running
        };
        assert_eq!(
            woken(
                Some(&running),
                false,
                HALT_RESTART_FLOOR,
                &Look {
                    held_to: Some(billed_only),
                    ..quiet()
                }
            ),
            None
        );
        let more_memory = shared::Allowances {
            memory_mib: running.memory_mib * 2,
            ..running
        };
        assert_eq!(
            woken(
                Some(&running),
                false,
                HALT_RESTART_FLOOR,
                &Look {
                    held_to: Some(more_memory),
                    ..quiet()
                }
            ),
            Some(Woken::Changed)
        );
        let halted = || Look {
            halted: true,
            ..quiet()
        };
        assert_eq!(
            woken(Some(&running), false, HALT_RESTART_FLOOR, &halted()),
            Some(Woken::Halted)
        );
        assert_eq!(
            woken(
                Some(&running),
                false,
                HALT_RESTART_FLOOR - Duration::from_millis(1),
                &halted()
            ),
            None,
            "a halt nobody has heard of woke a wait before the restart floor"
        );
        assert_eq!(
            woken(Some(&running), true, HALT_RESTART_FLOOR, &halted()),
            None,
            "a halt already known woke the wait"
        );
        assert_eq!(
            woken(
                Some(&running),
                true,
                HALT_RESTART_FLOOR,
                &Look {
                    finished: true,
                    ..halted()
                }
            ),
            Some(Woken::Finished)
        );
        assert_eq!(
            woken(
                Some(&running),
                false,
                HALT_RESTART_FLOOR,
                &Look {
                    replaced: true,
                    ..quiet()
                }
            ),
            Some(Woken::Replaced)
        );
    }

    #[test]
    fn a_failed_serve_is_a_restart_and_only_a_long_run_starts_the_count_again() {
        let now = Instant::now();
        let failed = Ended {
            failed: true,
            replaced: false,
            resumed: false,
        };
        let mut retry = Retry::default();
        assert!(retry.ended(failed, now));
        assert!(retry.ended(failed, now));
        assert!(retry.waiting(now + Duration::from_secs(9)));
        assert!(!retry.waiting(now + Duration::from_secs(10)));

        // However the serve ended, the count goes on.
        assert!(retry.ended(failed, now));
        assert!(retry.waiting(now + Duration::from_secs(19)));

        // One that ended well is no restart, and nothing is held off.
        let finished = Ended {
            failed: false,
            replaced: false,
            resumed: false,
        };
        assert!(!retry.ended(finished, now));
        assert!(!retry.waiting(now));

        // An agent that ran for a good while starts the count again.
        retry.failures.store(0, Ordering::SeqCst);
        assert!(retry.ended(failed, now));
        assert!(!retry.waiting(now + Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn a_vault_let_go_stops_its_agent() {
        let state = stand_in::chain(1_000_000_000, 0);
        let client = stand_in::client(&stand_in::start(state).await).await;
        let idle = || AbortOnDrop(tokio::spawn(std::future::pending()));
        let agent = tokio::spawn(std::future::pending::<Ended>());
        let watching = agent.abort_handle();
        let served = Served {
            shared: Shared {
                vault_key: Arc::new(keys::Key::fresh().unwrap()),
                attestation: crate::attestation::Attestation::new(
                    crate::attestation::Mode::Development,
                )
                .unwrap(),
                takes_over: Default::default(),
                api_url: Default::default(),
                holds: Default::default(),
                key: stand_in::key(&client),
                dry: Default::default(),
                metered: Default::default(),
                running: Default::default(),
                counters: Default::default(),
                watched_to: Default::default(),
                account_events: Default::default(),
                vault_events: Default::default(),
                wound_down: Default::default(),
                signed_through: Default::default(),
                approvals: Default::default(),
                ceilings: Default::default(),
                failed_starts: Default::default(),
                halt_known: Default::default(),
            },
            serving: Some(agent),
            retry: Retry::default(),
            _upkeep: [idle(), idle()],
            _leaf: no_leaf(),
            letting_go: None,
        };
        let mut running = HashMap::from([(Address::repeat_byte(0x11), served)]);

        running.remove(&Address::repeat_byte(0x11));
        tokio::task::yield_now().await;
        assert!(watching.is_finished(), "the agent still runs");
    }

    fn args(store: &std::path::Path) -> Args {
        Args {
            chain_args: chain::ChainArgs {
                network: chain::NetworkName::Local,
                chains: "config/chains.toml".into(),
                deployments_dir: ".".into(),
                vault_chain: None,
            },
            store: store.to_path_buf(),
            api_url: "http://127.0.0.1:9".to_string(),
            port: None,
            listen: IpAddr::from([127, 0, 0, 1]),
            attestation: attestation::Mode::Development,
            launcher_socket: attestation::LAUNCHER_SOCKET.into(),
            handover_silence_seconds: crate::handover::HANDOVER_SILENCE,
            store_url: None,
            ceilings: Default::default(),
            hyperliquid_stream: None,
            root_peers: Vec::new(),
            no_root_key: false,
        }
    }

    /// A leaf in a tree nobody reads.
    fn no_leaf() -> Leaf {
        Leaf {
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            chain_id: 0,
            vault: Address::ZERO,
            key: Address::ZERO,
        }
    }

    /// One vault's part of a sweep, with an attestation of its own.
    #[allow(clippy::too_many_arguments)]
    async fn sweep_vault(
        chain: shared::Chain,
        client: &Client,
        deployments: &Deployments,
        native_usd: Option<f64>,
        engine: &Engine,
        args: &Args,
        totals: &usage::Totals,
        book: &mut Book,
        vault: Address,
    ) -> Result<()> {
        let attestation =
            crate::attestation::Attestation::new(crate::attestation::Mode::Development)?;
        sweep_one(
            chain,
            client,
            deployments,
            native_usd,
            engine,
            args,
            totals,
            &attestation,
            book,
            vault,
        )
        .await
    }

    /// The factory's list of vaults is read whole, so it isn't read at every
    /// sweep: once, then again only once [`VAULT_LIST_EVERY`] has passed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_list_of_vaults_is_read_again_only_every_so_often() {
        let state = stand_in::chain(1_000_000_000, 0);
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let store = std::env::temp_dir().join(format!("executor-list-{}", std::process::id()));
        let engine = runner::engine().unwrap();
        let mut book = Book::default();
        let reads = || state.lock().unwrap().vault_list_reads;
        let sweep_now = async |book: &mut Book| {
            sweep(
                shared::Chain::Base,
                &client,
                &stand_in::deployments(),
                None,
                &engine,
                &args(&store),
                &Default::default(),
                &crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                    .unwrap(),
                book,
            )
            .await
            .unwrap()
        };
        sweep_now(&mut book).await;
        sweep_now(&mut book).await;
        sweep_now(&mut book).await;
        assert_eq!(reads(), 1, "the list was read at every sweep");
        book.vaults_read = book.vaults_read.map(|at| at - VAULT_LIST_EVERY);
        sweep_now(&mut book).await;
        assert_eq!(reads(), 2, "the list was never read again");
    }

    /// Take vault 0x11 on, as a sweep does.
    async fn staked_and_taken_on(client: &Client, store: &std::path::Path) -> Served {
        let vault = Address::repeat_byte(0x11);
        let deployments = stand_in::deployments();
        let head = client.block_number().await.unwrap();
        let bundle = client.read(vault, IVault::bundleHashCall {}).await.unwrap();
        take_on(
            client,
            shared::Chain::Base,
            &deployments,
            None,
            vault,
            bundle,
            head,
            None,
            &args(store),
            &Default::default(),
            &crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap(),
            0,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_agent_started_after_the_halt_is_told_of_it_before_anything_else() {
        use watch::VaultEvent;
        let vault = Address::repeat_byte(0x11);
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().head = 20;
        state.lock().unwrap().halted = Some((vault, 15));
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let heard = |feed: &feed::Feed<VaultEvent>| {
            let reader = feed.reader();
            let mut reader = reader.lock().unwrap();
            std::iter::from_fn(|| reader.try_recv().ok()).collect::<Vec<_>>()
        };

        // The agent before left news of a shortfall unread, and of the halt
        // too, which is not said twice.
        let events = feed::Feed::<VaultEvent>::default();
        events.sender().send(VaultEvent::BufferLow(5)).unwrap();
        events
            .sender()
            .send(VaultEvent::Halting("the vault was stopped".to_string()))
            .unwrap();
        tell_if_halted(&client, vault, &events).await.unwrap();
        let heard_first = heard(&events);
        assert!(
            matches!(
                heard_first.as_slice(),
                [VaultEvent::Halting(why), VaultEvent::BufferLow(5)] if why == "the vault was stopped"
            ),
            "the agent is not told of the halt first, and once"
        );

        // A vault that hasn't halted: nothing to tell.
        state.lock().unwrap().halted = None;
        let events = feed::Feed::<VaultEvent>::default();
        tell_if_halted(&client, vault, &events).await.unwrap();
        assert!(heard(&events).is_empty());
    }

    #[test]
    fn a_warning_is_said_at_most_once_a_minute() {
        let now = Instant::now();
        let mut throttle = Throttle::default();
        assert!(throttle.due(now));
        assert!(!throttle.due(now + Duration::from_secs(59)));
        assert!(throttle.due(now + Duration::from_secs(60)));
    }

    /// Serve a vault against the serving stand-in until the serve stops at
    /// the strategy's id, which the stand-in doesn't answer.
    async fn serve_until_the_strategy(client: Client, key: chain::KeyClient) {
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key,
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        let served = tokio::time::timeout(
            Duration::from_secs(60),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                std::env::temp_dir(),
                "local".to_string(),
                shared,
            ),
        )
        .await
        .expect("the serve went no further than the strategy's id");
        assert!(served.is_err(), "the stand-in reverts the strategy's id");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_the_keeper_blessed_again_is_approved_again_at_the_venue_and_never_blesses_itself()
     {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            // The key was approved until its old blessing's end; the keeper
            // has blessed it again since.
            let mut node = node.lock().unwrap();
            node.approved_key = key.address;
            node.blessed_until = NOW + TTL;
            node.venue_until = NOW + TTL / 4;
        }
        serve_until_the_strategy(client, key).await;
        let node = node.lock().unwrap();
        assert_eq!(
            node.venue_until, node.blessed_until,
            "the venue holds the key approved only until the old blessing's end"
        );
        assert!(
            node.sent.iter().all(|(_, selector)| *selector
                != <IAttestationRegistry::blessCall as alloy::sol_types::SolCall>::SELECTOR),
            "the executor sent a blessing itself"
        );
    }

    /// A serve sends nothing until the keeper has blessed its key, and says
    /// meanwhile, in the instance's tree, that the key wants blessing; once
    /// the chain shows it blessed (and stocked), the key is approved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_serve_waits_for_the_keeper_s_blessing_before_it_sends_anything() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().blessed_until = 0;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        let attestation =
            crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap();
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: attestation.clone(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key: key.clone(),
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        let serving = tokio::spawn(serve(
            client,
            shared::Chain::Base,
            stand_in::deployments(),
            runner::engine().unwrap(),
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            std::env::temp_dir(),
            "local".to_string(),
            shared,
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            node.lock().unwrap().sent.is_empty(),
            "a key not yet blessed sent something"
        );
        let leaf = |attestation: &crate::attestation::Attestation| {
            attestation
                .status()
                .leaves
                .into_iter()
                .find(|leaf| leaf.key.parse::<Address>().unwrap() == key.address)
        };
        let held = leaf(&attestation).expect("the key is not in the instance's tree");
        assert_eq!(
            held.vault.parse::<Address>().unwrap(),
            Address::repeat_byte(0x11)
        );
        assert_eq!(
            held.strategy_hash.parse::<B256>().unwrap(),
            B256::repeat_byte(0x42)
        );
        assert!(attestation.status().leaves.len() == 1);

        // The keeper blesses it.
        node.lock().unwrap().blessed_until = NOW + TTL;
        let served = tokio::time::timeout(Duration::from_secs(30), serving)
            .await
            .expect("the serve never went on once the key was blessed")
            .unwrap();
        assert!(served.is_err(), "the stand-in reverts the strategy's id");
        assert_eq!(stand_in::serving::approvals(&node), 1);
        assert_eq!(node.lock().unwrap().approved_key, key.address);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_whose_venue_approval_already_follows_its_blessing_is_not_approved_again() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            // Blessed for a while, and approved until exactly then.
            let mut node = node.lock().unwrap();
            node.approved_key = key.address;
            node.blessed_until = NOW + TTL * 3 / 4;
            node.venue_until = NOW + TTL * 3 / 4;
        }
        serve_until_the_strategy(client, key).await;
        assert_eq!(
            stand_in::serving::approvals(&node),
            0,
            "an approval that changes nothing was paid for"
        );
        assert!(
            node.lock().unwrap().sent.is_empty(),
            "a key blessed for a while was blessed again"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_approval_that_fails_after_a_renewal_is_tried_again_until_it_lands() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.failing_approvals = 2;
        }
        let landed = tokio::time::timeout(
            Duration::from_secs(30),
            approve_until_it_lands(
                &client,
                &stand_in::deployments(),
                &crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                    .unwrap(),
                &key,
                station,
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                &Default::default(),
            ),
        )
        .await
        .expect("the approval never landed");
        assert_eq!(landed, Held::Kept);
        assert_eq!(stand_in::serving::approvals(&node), 3);
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
    }

    /// Take a killed vault on, `halted_secs_ago` into a ten-minute grace, and
    /// say whether it was.
    async fn a_killed_vault_is_taken_on(halted_secs_ago: u64) -> bool {
        use stand_in::serving::NOW;
        let node = stand_in::serving::node(VaultState::Killed);
        node.lock().unwrap().halted_at = NOW - halted_secs_ago;
        let bundle = node.lock().unwrap().bundle;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let store = std::env::temp_dir().join(format!(
            "executor-killed-{}-{halted_secs_ago}",
            std::process::id()
        ));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();
        let vault = Address::repeat_byte(0x11);
        let mut book = Book::default();
        sweep_vault(
            shared::Chain::Base,
            &client,
            &stand_in::deployments(),
            None,
            &runner::engine().unwrap(),
            &args(&store),
            &Default::default(),
            &mut book,
            vault,
        )
        .await
        .unwrap();
        let taken_on = book.running.contains_key(&vault);
        drop(book);
        let _ = std::fs::remove_dir_all(&store);
        taken_on
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vault_killed_and_still_in_its_grace_is_served_after_a_restart() {
        assert!(
            a_killed_vault_is_taken_on(60).await,
            "a killed vault with nine minutes of grace left was not served"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_killed_vault_whose_grace_is_over_is_not_served() {
        assert!(!a_killed_vault_is_taken_on(601).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_venue_approval_that_lags_its_blessing_is_caught_up_while_the_serve_runs() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            // Renewed on the chain, and nothing told the venue: no renewal
            // is seen from here.
            let mut node = node.lock().unwrap();
            node.approved_key = key.address;
            node.blessed_until = NOW + TTL;
            node.venue_until = NOW + TTL / 4;
        }
        let following = tokio::spawn(follow_blessing(
            client,
            stand_in::deployments(),
            crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap(),
            key,
            station,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            Default::default(),
            Default::default(),
            Default::default(),
        ));
        for _ in 0..50 {
            if node.lock().unwrap().venue_until == NOW + TTL {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // A few more looks, which find nothing to do.
        tokio::time::sleep(FOLLOW_EVERY * 3).await;
        following.abort();
        assert_eq!(
            node.lock().unwrap().venue_until,
            NOW + TTL,
            "the venue still holds the key approved until the old blessing's end"
        );
        assert_eq!(
            stand_in::serving::approvals(&node),
            1,
            "an approval that already follows its blessing was paid for again"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_venue_approval_is_followed_at_once_after_a_renewal() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            let mut node = node.lock().unwrap();
            node.approved_key = key.address;
            node.blessed_until = NOW + TTL;
            node.venue_until = NOW + TTL / 4;
            // The first approval reverts and is tried again.
            node.failing_approvals = 1;
        }
        let renewed = Arc::new(tokio::sync::Notify::new());
        let following = tokio::spawn(follow_blessing(
            client,
            stand_in::deployments(),
            crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap(),
            key,
            station,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            Default::default(),
            renewed.clone(),
            Default::default(),
        ));
        renewed.notify_one();
        for _ in 0..50 {
            if node.lock().unwrap().venue_until == NOW + TTL {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        following.abort();
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
        assert_eq!(stand_in::serving::approvals(&node), 2);
    }

    #[test]
    fn only_an_agent_that_ended_cleanly_once_told_of_the_halt_has_done_its_part() {
        assert!(done_its_part(true, true));
        // Ended cleanly before it was told: started again, and told.
        assert!(!done_its_part(true, false));
        // Faulted, or never started, whatever it was told.
        assert!(!done_its_part(false, true));
        assert!(!done_its_part(false, false));
    }

    /// A killed vault's shared state, with its key blessed and approved, and
    /// a store of its own holding `component` as its bundle: tests running at
    /// once each write theirs.
    async fn a_killed_vault(
        wound_down: bool,
        component: &[u8],
    ) -> (stand_in::killed::Shared, Client, Shared, std::path::PathBuf) {
        static STORES: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "executor-killed-serve-{}-{wound_down}-{}",
            std::process::id(),
            STORES.fetch_add(1, Ordering::SeqCst)
        ));
        let manifest_hash = stand_in::killed::store(&dir, component);
        let node = Arc::new(Mutex::new(stand_in::killed::Killed {
            key: Address::ZERO,
            manifest_hash,
            agent_starts: 0,
            failing_reads: 0,
            approved_instead: None,
            allowances: shared::Allowances::modest(),
        }));
        let client = stand_in::killed::start(node.clone()).await;
        let key = stand_in::key(&client);
        node.lock().unwrap().key = key.address;
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key,
            dry: Default::default(),
            metered: Arc::new(AtomicBool::new(true)),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Arc::new(AtomicBool::new(wound_down)),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        (node, client, shared, dir)
    }

    /// Serve the killed vault for a while, and say how many times its agent
    /// was started.
    async fn serve_killed_for(wound_down: bool, secs: u64) -> usize {
        let (node, client, shared, dir) =
            a_killed_vault(wound_down, stand_in::killed::NOT_A_COMPONENT).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(secs),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir.clone(),
                "local".to_string(),
                shared,
            ),
        )
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        node.lock().unwrap().agent_starts
    }

    /// A component whose start function never returns: its `run` is never
    /// reached.
    const SPINS_AT_INSTANTIATION: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func $spin (loop $on (br $on)))
        (start $spin)
        (func (export "run") (result i32) (i32.const 0)))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

    /// Serve the killed vault with `component` as its bundle for `secs`, and
    /// say whether the agent was started and whether it was billed as
    /// running at any moment.
    async fn billed_serving(component: &[u8], secs: u64) -> (bool, bool) {
        let (node, client, shared, dir) = a_killed_vault(false, component).await;
        let running = shared.running.clone();
        let serving = serve(
            client,
            shared::Chain::Base,
            stand_in::deployments(),
            runner::engine().unwrap(),
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            dir.clone(),
            "local".to_string(),
            shared,
        );
        let watching = async {
            loop {
                if running.load(Ordering::SeqCst) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let billed = tokio::select! {
            served = serving => panic!("the serve ended: {:?}", served.map_err(|e| format!("{e:#}"))),
            billed = watching => billed,
            _ = tokio::time::sleep(Duration::from_secs(secs)) => false,
        };
        let _ = std::fs::remove_dir_all(&dir);
        let started = node.lock().unwrap().agent_starts > 0;
        (started, billed)
    }

    /// Instantiating a component runs its own code (every module's start
    /// function, a Rust strategy's constructors among them), so an agent
    /// being instantiated is billed as running: here its start function
    /// spins, and its `run` is never reached.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_being_instantiated_is_billed_as_running() {
        let (started, billed) = billed_serving(SPINS_AT_INSTANTIATION.as_bytes(), 4).await;
        assert!(started, "the agent was never started");
        assert!(billed, "an agent running its start function was not billed");
    }

    /// Compiling a component is the host's work and runs none of the
    /// strategy's code: one that never gets past it is never billed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_never_compiles_is_never_billed() {
        let (started, billed) = billed_serving(stand_in::killed::NOT_A_COMPONENT, 4).await;
        assert!(started, "the agent was never started");
        assert!(
            !billed,
            "an agent that never compiled was billed as running"
        );
    }

    /// Instantiating is given only so long: an agent that takes longer is
    /// stopped as a fault, billed as running only for that time, and its
    /// `run` is never reached.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_does_not_finish_instantiating_is_stopped_as_a_fault() {
        let (_node, client, shared, dir) =
            a_killed_vault(false, SPINS_AT_INSTANTIATION.as_bytes()).await;
        let running = shared.running.clone();
        let runner = runner::Runner {
            place: host::Place::Evm(host::Evm {
                chain: client,
                key: shared.key.clone(),
                vault: Address::repeat_byte(0x11),
                station: Address::repeat_byte(0x55),
                venue: Address::ZERO,
                usd: Address::ZERO,
                factory: Address::ZERO,
                hyperliquid: None,
                stocks: None,
                lighter: None,
            }),
            home_chain: "base".to_string(),
            drivers: vec!["local".to_string()],
            models: None,
            network: false,
            allowances: shared::Allowances::modest(),
            halting_heard: Default::default(),
            running: running.clone(),
            instantiate_within: Duration::from_secs(1),
            instantiated: Default::default(),
            following: Default::default(),
        };
        let billed = Arc::new(AtomicBool::new(false));
        let watching = {
            let (running, billed) = (running.clone(), billed.clone());
            AbortOnDrop(tokio::spawn(async move {
                loop {
                    if running.load(Ordering::SeqCst) {
                        billed.store(true, Ordering::SeqCst);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }))
        };
        let stopped = tokio::time::timeout(
            Duration::from_secs(10),
            runner.run(
                &runner::engine().unwrap(),
                SPINS_AT_INSTANTIATION.as_bytes(),
                shared.account_events.reader(),
                shared.vault_events.reader(),
                shared.counters.clone(),
            ),
        )
        .await
        .expect("an agent that never finished instantiating was never stopped")
        .unwrap();
        drop(watching);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(stopped, runner::Stopped::NotInstantiated);
        assert_eq!(shared.counters.lock().unwrap().faults, 1);
        assert!(
            billed.load(Ordering::SeqCst),
            "its start function was not billed"
        );
        assert!(
            !running.load(Ordering::SeqCst),
            "it is still billed as running"
        );
    }

    /// An agent that faults in the grace (here its component won't load, as
    /// an executor restarted in the grace whose first start failed) is
    /// started again five seconds later, as outside a halt, so it can still
    /// be told and wind down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_faults_in_the_grace_is_started_again() {
        assert!(
            serve_killed_for(false, 8).await >= 2,
            "an agent that stopped a minute into a ten-minute grace was never started again"
        );
    }

    /// An agent that wound down in the halt, whose serve was then ended by a
    /// read that failed while it waited out the grace, is not started again
    /// by the serve that follows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_wound_down_in_the_grace_is_not_started_again() {
        assert_eq!(
            serve_killed_for(true, 4).await,
            0,
            "an agent that had done its part was started again"
        );
    }

    /// A component that takes `pages` of memory as it starts and then runs
    /// until it is stopped, or, refused them, ends with an error: whether it
    /// runs says what memory allowance it was started under.
    fn an_agent_needing(pages: u32) -> String {
        format!(
            r#"(component
                (core module $m
                    (memory (export "memory") 1)
                    (data (i32.const 16) "\01\00\00\00\20\00\00\00\0a\00\00\00")
                    (data (i32.const 32) "no memory!")
                    (func (export "run") (result i32)
                        (if (i32.eq (memory.grow (i32.const {pages})) (i32.const -1))
                            (then (return (i32.const 16))))
                        (loop $on (br $on))
                        (i32.const 0)))
                (core instance $i (instantiate $m))
                (func $run async (result (result (error string)))
                    (canon lift (core func $i "run") (memory (core memory $i "memory"))))
                (export "run" (func $run)))"#
        )
    }

    /// The author lowers the memory allowance while the agent runs: the
    /// agent is started again under the new one at once (within a look at
    /// the allowances, two seconds), not left running under the old one
    /// until it next stops of its own accord. Here the new allowance is too
    /// small for the agent, so it faults as soon as it is started under it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_allowance_change_reaches_a_running_agent_at_once() {
        let (node, client, shared, dir) =
            a_killed_vault(false, an_agent_needing(512).as_bytes()).await;
        // 32 MiB as it starts, under 64.
        node.lock().unwrap().allowances = shared::Allowances {
            memory_mib: 64,
            ..shared::Allowances::modest()
        };
        let counters = shared.counters.clone();
        let running = shared.running.clone();
        let store = dir.clone();
        let serving = AbortOnDrop(tokio::spawn(async move {
            let _ = serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir,
                "local".to_string(),
                shared,
            )
            .await;
        }));
        let faults = || counters.lock().unwrap().faults;

        // The agent runs under the allowance it was started with.
        turn::wait_for("the agent to run", || running.load(Ordering::SeqCst)).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(running.load(Ordering::SeqCst));
        assert_eq!(faults(), 0, "the agent didn't run under 64 MiB");

        // Down to 16 MiB: the running agent is stopped and started again
        // under it, where it can't have its 32.
        node.lock().unwrap().allowances.memory_mib = 16;
        let changed = Instant::now();
        turn::wait_for("the agent to be started under the new allowance", || {
            faults() > 0
        })
        .await;
        assert!(
            changed.elapsed() < Duration::from_secs(5),
            "the change reached the agent only after {:?}",
            changed.elapsed()
        );
        drop(serving);
        let _ = std::fs::remove_dir_all(&store);
    }

    /// An agent that faults as soon as it runs is held off for longer each
    /// time (twenty seconds by its third fault). The author raises the
    /// allowance it faults for: the agent is started under the new one
    /// within a look at the allowances, not once the wait is out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_allowance_change_cuts_short_the_wait_to_restart_a_faulting_agent() {
        let (node, client, shared, dir) =
            a_killed_vault(false, an_agent_needing(512).as_bytes()).await;
        // 32 MiB as it starts, under 16: it faults at once.
        node.lock().unwrap().allowances = shared::Allowances {
            memory_mib: 16,
            ..shared::Allowances::modest()
        };
        let counters = shared.counters.clone();
        let running = shared.running.clone();
        let store = dir.clone();
        let serving = AbortOnDrop(tokio::spawn(async move {
            let _ = serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir,
                "local".to_string(),
                shared,
            )
            .await;
        }));
        let faults = || counters.lock().unwrap().faults;
        let started = Instant::now();
        while faults() < 3 {
            assert!(
                started.elapsed() < Duration::from_secs(40),
                "the agent faulted only {} times",
                faults()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Up to 64 MiB, in the twenty seconds' wait.
        node.lock().unwrap().allowances.memory_mib = 64;
        let changed = Instant::now();
        while !running.load(Ordering::SeqCst) {
            assert!(
                changed.elapsed() < Duration::from_secs(6),
                "the agent was not started under the new allowance until its wait was out"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(serving);
        let _ = std::fs::remove_dir_all(&store);
        assert_eq!(faults(), 3, "the agent faulted under the new allowance");
    }

    /// The host holds an agent to its fuel and its memory, and to nothing
    /// else of its allowances: a change to those starts it again, a change
    /// to anything else (gas, requests, bytes, streams, inference) leaves
    /// it running, and the meter bills the new figures as it reads them.
    #[test]
    fn only_a_change_to_what_the_host_holds_the_agent_to_starts_it_again() {
        let running = shared::Allowances::modest();
        let billed_only = shared::Allowances {
            gas_per_day: running.gas_per_day * 10.0,
            requests_per_min: running.requests_per_min * 2,
            bytes_per_day: running.bytes_per_day * 2,
            streams: running.streams + 1,
            inference_ms_per_min: running.inference_ms_per_min + 1_000,
            ..running
        };
        assert!(!restarts_under(&running, &billed_only));
        let memory = shared::Allowances {
            memory_mib: running.memory_mib / 2,
            ..running
        };
        assert!(restarts_under(&running, &memory));
        let fuel = shared::Allowances {
            fuel_per_sec: running.fuel_per_sec * 2,
            ..running
        };
        assert!(restarts_under(&running, &fuel));
    }

    /// The author raises the gas allowance, and the others the host doesn't
    /// hold the agent to, while it runs: the agent runs on, never stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_allowance_change_the_host_does_not_enforce_leaves_the_agent_running() {
        let (node, client, shared, dir) =
            a_killed_vault(false, an_agent_needing(1).as_bytes()).await;
        let running = shared.running.clone();
        let store = dir.clone();
        let serving = AbortOnDrop(tokio::spawn(async move {
            let _ = serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir,
                "local".to_string(),
                shared,
            )
            .await;
        }));
        turn::wait_for("the agent to run", || running.load(Ordering::SeqCst)).await;

        // Watched without pause, so even a moment's stop is seen.
        let watched = Arc::new(AtomicBool::new(true));
        let stopped = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (running, watched, stopped) = (running.clone(), watched.clone(), stopped.clone());
            std::thread::spawn(move || {
                while watched.load(Ordering::SeqCst) {
                    if !running.load(Ordering::SeqCst) {
                        stopped.store(true, Ordering::SeqCst);
                    }
                }
            })
        };
        {
            let mut node = node.lock().unwrap();
            node.allowances.gas_per_day *= 10.0;
            node.allowances.requests_per_min *= 2;
            node.allowances.bytes_per_day *= 2;
            node.allowances.streams += 1;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        watched.store(false, Ordering::SeqCst);
        watcher.join().unwrap();
        drop(serving);
        let _ = std::fs::remove_dir_all(&store);
        assert!(
            !stopped.load(Ordering::SeqCst),
            "the agent was stopped for a change the host doesn't hold it to"
        );
    }

    /// Whoever can write the store swaps the vault's component for another
    /// that would run: it is never started under the vault's key, the time
    /// is not billed as running, and the refusal is counted for the health
    /// report. The serve ends, and is tried again later.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_component_swapped_in_the_store_is_not_run_nor_billed() {
        let (_node, client, shared, dir) =
            a_killed_vault(false, an_agent_needing(1).as_bytes()).await;
        std::fs::write(
            bundle_path(&dir, B256::repeat_byte(0x42)),
            an_agent_needing(2),
        )
        .unwrap();
        let counters = shared.counters.clone();
        let running = shared.running.clone();
        let served = tokio::time::timeout(
            Duration::from_secs(10),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir.clone(),
                "local".to_string(),
                shared,
            ),
        )
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        let error = served
            .expect("the serve ended")
            .expect_err("the serve refused the component");
        assert!(error.is::<NotTheBundle>(), "{error:#}");
        assert!(!running.load(Ordering::SeqCst));
        assert_eq!(counters.lock().unwrap().not_the_bundle, 1);
        assert_eq!(counters.lock().unwrap().faults, 0);
    }

    /// A serve that fails is started again by the sweeps that follow, while
    /// what runs for as long as the vault does (its meter, its reports and
    /// its key's top-ups) is started once, when the vault is taken on, and
    /// not again with each serve: two meters for one vault would sign two
    /// totals, the later one short of the first.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_serve_started_again_leaves_one_meter_for_the_vault() {
        let node = stand_in::serving::node(VaultState::Open);
        let bundle = node.lock().unwrap().bundle;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let store = std::env::temp_dir().join(format!("executor-one-meter-{}", std::process::id()));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();
        let vault = Address::repeat_byte(0x11);
        let args = args(&store);
        let engine = runner::engine().unwrap();
        let totals: usage::Totals = Default::default();
        let mut book = Book::default();
        let ids = |book: &Book| {
            let served = &book.running[&vault];
            served._upkeep.each_ref().map(|task| task.0.id())
        };

        // Each serve stops at the strategy's id, which the stand-in doesn't
        // answer, and is started again once its wait is over.
        let mut serves = HashSet::new();
        let mut first = None;
        let started = Instant::now();
        while serves.len() < 3 && started.elapsed() < Duration::from_secs(30) {
            sweep_vault(
                shared::Chain::Base,
                &client,
                &stand_in::deployments(),
                Some(2_500.0),
                &engine,
                &args,
                &totals,
                &mut book,
                vault,
            )
            .await
            .unwrap();
            assert_eq!(book.running.len(), 1);
            let ids = ids(&book);
            assert_eq!(
                *first.get_or_insert(ids),
                ids,
                "the meter or the reports were started again"
            );
            if let Some(serving) = &book.running[&vault].serving {
                serves.insert(serving.id());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            serves.len() >= 3,
            "the serve was started {} times",
            serves.len()
        );
        assert!(
            book.running[&vault]
                ._upkeep
                .iter()
                .all(|task| !task.0.is_finished()),
            "the meter or the reports stopped"
        );

        // One meter signs once a tick (half a second here), reading the tank
        // each time: two would read it twice as often. A report may read it
        // once more.
        let before = node.lock().unwrap().tank_reads;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let signed = node.lock().unwrap().tank_reads - before;
        assert!(
            (2..=9).contains(&signed),
            "{signed} totals signed in three seconds, at a tick every half second"
        );
        drop(book);
        let _ = std::fs::remove_dir_all(&store);
    }

    /// Follow a key's blessing against the serving stand-in for a few looks,
    /// with `other` approved at the station, and say how many approvals were
    /// sent.
    async fn follow_with_another_key_approved(
        lapsed: bool,
    ) -> (stand_in::serving::Shared, Address, bool) {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        let other = Address::repeat_byte(0x77);
        {
            // This key is still blessed; the station approves another, and
            // withdrew this key's venue approval when it did.
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.approved_key = other;
            node.venue_until = 0;
            if lapsed {
                node.lapsed.insert(other);
            }
        }
        let replaced = Arc::new(AtomicBool::new(false));
        let following = tokio::spawn(follow_blessing(
            client,
            stand_in::deployments(),
            crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap(),
            key.clone(),
            station,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            Default::default(),
            Default::default(),
            replaced.clone(),
        ));
        tokio::time::sleep(FOLLOW_EVERY * 5).await;
        following.abort();
        (node, key.address, replaced.load(Ordering::SeqCst))
    }

    /// One executor at a time: another executor's key, still blessed, has
    /// taken the station's approval over. Taking it back would revoke the
    /// successor, which would take it back in turn at its next look, the two
    /// paying for an approval each per minute and each agent's orders
    /// refused half the time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_replaced_at_the_station_does_not_take_the_approval_back() {
        let (node, _, replaced) = follow_with_another_key_approved(false).await;
        assert!(replaced, "the serve was not told it has been replaced");
        assert_eq!(
            stand_in::serving::approvals(&node),
            0,
            "a key that was replaced at the station sent approveKey to take the vault back"
        );
        assert_eq!(
            node.lock().unwrap().approved_key,
            Address::repeat_byte(0x77)
        );
    }

    /// A key whose blessing has lapsed holds nothing: the station replaces it
    /// with whichever blessed key asks, and this one does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lapsed_key_at_the_station_is_replaced() {
        let (node, key, replaced) = follow_with_another_key_approved(true).await;
        assert!(!replaced);
        assert_eq!(stand_in::serving::approvals(&node), 1);
        assert_eq!(node.lock().unwrap().approved_key, key);
    }

    /// The same at a serve's start: a serve started after a handover doesn't
    /// take the approval back either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_serve_started_after_a_handover_does_not_take_the_approval_back() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            // This key's blessing is nearly over, and another executor's
            // key, still blessed, holds the approval.
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL / 4;
            node.approved_key = Address::repeat_byte(0x77);
        }
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key,
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        let served = serve(
            client,
            shared::Chain::Base,
            stand_in::deployments(),
            runner::engine().unwrap(),
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            std::env::temp_dir(),
            "local".to_string(),
            shared,
        )
        .await
        .unwrap();
        assert_eq!(served, Stop::Replaced);
        assert_eq!(
            node.lock().unwrap().agent_starts,
            0,
            "the agent was started"
        );
        assert!(
            node.lock().unwrap().sent.is_empty(),
            "the replaced key was blessed or approved again: {:?}",
            node.lock().unwrap().sent
        );
        assert_eq!(
            node.lock().unwrap().approved_key,
            Address::repeat_byte(0x77)
        );
    }

    /// An approval whose send failed but landed all the same (the node's
    /// reply was lost) is not sent again: where the approval stands is read
    /// again before each new try.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_approval_that_landed_though_its_send_failed_is_not_sent_again() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.unanswered_approvals = 1;
        }
        let landed = tokio::time::timeout(
            Duration::from_secs(30),
            approve_until_it_lands(
                &client,
                &stand_in::deployments(),
                &crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                    .unwrap(),
                &key,
                station,
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                &Default::default(),
            ),
        )
        .await
        .expect("the approval was never seen to land");
        assert_eq!(landed, Held::Kept);
        assert_eq!(
            stand_in::serving::approvals(&node),
            1,
            "an approval that had landed was sent again"
        );
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
    }

    /// An agent that ended cleanly after hearing the halt has wound down,
    /// and stays so though the read that follows fails and ends the serve:
    /// the serve started after it doesn't start the agent again in the
    /// grace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_wound_down_stays_wound_down_after_a_read_that_fails() {
        let (node, client, shared, dir) =
            a_killed_vault(false, stand_in::killed::NOT_A_COMPONENT).await;
        let vault = Address::repeat_byte(0x11);
        node.lock().unwrap().failing_reads = 1;
        let after = after_the_agent(&client, vault, &shared.wound_down, true).await;
        assert!(after.is_err(), "the read after the agent failed");
        assert!(shared.wound_down.load(Ordering::SeqCst));

        let _ = tokio::time::timeout(
            Duration::from_secs(4),
            serve(
                client.clone(),
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                vault,
                B256::repeat_byte(0x42),
                dir.clone(),
                "local".to_string(),
                shared,
            ),
        )
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            node.lock().unwrap().agent_starts,
            0,
            "an agent that had wound down was started again"
        );

        // One that faulted, or ended before it was told, has not wound down.
        let wound_down = AtomicBool::new(false);
        assert!(
            !after_the_agent(&client, vault, &wound_down, false)
                .await
                .unwrap()
        );
        assert!(!wound_down.load(Ordering::SeqCst));
    }

    /// A halted vault past its grace has its key's top-ups stopped (the
    /// station is stopped, and the key has nothing left to send), while its
    /// meter runs on so the final bill can be collected.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_halted_vault_past_its_grace_keeps_its_meter() {
        use stand_in::serving::NOW;
        let node = stand_in::serving::node(VaultState::Killed);
        node.lock().unwrap().halted_at = NOW - 3_600;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let store = std::env::temp_dir().join(format!("executor-let-go-{}", std::process::id()));

        /// A task that runs until it is stopped, and lowers its flag then.
        fn flagged() -> (AbortOnDrop, Arc<AtomicBool>) {
            struct Lowers(Arc<AtomicBool>);
            impl Drop for Lowers {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let flag = Arc::new(AtomicBool::new(true));
            let lowers = Lowers(flag.clone());
            let task = tokio::spawn(async move {
                let _lowers = lowers;
                std::future::pending::<()>().await
            });
            (AbortOnDrop(task), flag)
        }
        let (metering, meter_runs) = flagged();
        let idle = || AbortOnDrop(tokio::spawn(std::future::pending()));
        let served = Served {
            shared: Shared {
                vault_key: Arc::new(keys::Key::fresh().unwrap()),
                attestation: crate::attestation::Attestation::new(
                    crate::attestation::Mode::Development,
                )
                .unwrap(),
                takes_over: Default::default(),
                api_url: Default::default(),
                holds: Default::default(),
                key: stand_in::key(&client),
                dry: Default::default(),
                metered: Default::default(),
                running: Default::default(),
                counters: Default::default(),
                watched_to: Default::default(),
                account_events: Default::default(),
                vault_events: Default::default(),
                wound_down: Default::default(),
                signed_through: Default::default(),
                approvals: Default::default(),
                ceilings: Default::default(),
                failed_starts: Default::default(),
                halt_known: Default::default(),
            },
            serving: None,
            retry: Default::default(),
            _upkeep: [metering, idle()],
            _leaf: no_leaf(),
            letting_go: None,
        };
        let mut book = Book::default();
        book.running.insert(vault, served);

        sweep_vault(
            shared::Chain::Base,
            &client,
            &stand_in::deployments(),
            None,
            &runner::engine().unwrap(),
            &args(&store),
            &Default::default(),
            &mut book,
            vault,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = std::fs::remove_dir_all(&store);
        assert!(
            meter_runs.load(Ordering::SeqCst),
            "the meter stopped before the final bill was collected"
        );
        assert!(book.running.contains_key(&vault));
    }

    /// The vault's events are read from the block it was taken on at, so an
    /// agent first started later still hears what happened in between.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vaults_events_are_read_from_the_block_it_was_taken_on_at() {
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().head = 16;
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        // The key already holds a full stake, so none is sent.
        let stake = alloy::primitives::U256::from(health::stake_today(&client).await.unwrap());
        state.lock().unwrap().sent.push(stake);
        let store = std::env::temp_dir().join(format!("executor-watched-{}", std::process::id()));
        let served = staked_and_taken_on(&client, &store).await;
        assert_eq!(served.shared.watched_to.load(Ordering::SeqCst), 16);
    }

    /// A serve whose key another executor's key, still blessed, replaces at
    /// the station while its agent runs stops the agent there (its capacity
    /// is billed no longer) and ends, saying so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_serve_whose_key_is_replaced_stops_its_agent_and_ends() {
        let (node, client, shared, dir) =
            a_killed_vault(false, stand_in::killed::NOT_A_COMPONENT).await;
        let running = shared.running.clone();
        let handover = {
            let node = node.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                node.lock().unwrap().approved_instead = Some(Address::repeat_byte(0x77));
            }
        };
        let serving = tokio::time::timeout(
            Duration::from_secs(10),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                dir.clone(),
                "local".to_string(),
                shared,
            ),
        );
        let (served, ()) = tokio::join!(serving, handover);
        let _ = std::fs::remove_dir_all(&dir);
        let served = served.expect("the serve carried on after it was replaced");
        assert_eq!(served.unwrap(), Stop::Replaced);
        assert!(
            !running.load(Ordering::SeqCst),
            "the agent is still billed as running"
        );
    }

    /// A replaced executor lets the vault go: its key's top-ups stop at once,
    /// and the vault (its meter with it) is dropped once the meter has signed
    /// a total counting up to the agent's stop. It isn't taken on again while
    /// the other executor's key stands, and is once that key's blessing
    /// lapses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replaced_executor_lets_the_vault_go_until_the_other_key_lapses() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let bundle = node.lock().unwrap().bundle;
        let other = Address::repeat_byte(0x77);
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.approved_key = other;
        }
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let store = std::env::temp_dir().join(format!("executor-replaced-{}", std::process::id()));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();
        let sweep = async |book: &mut Book| {
            sweep_vault(
                shared::Chain::Base,
                &client,
                &stand_in::deployments(),
                None,
                &runner::engine().unwrap(),
                &args(&store),
                &Default::default(),
                book,
                vault,
            )
            .await
            .unwrap();
        };

        // A serve that has ended for a handover.
        let idle = || AbortOnDrop(tokio::spawn(std::future::pending()));
        let signed_through = usage::SignedThrough::default();
        let served = Served {
            shared: Shared {
                vault_key: Arc::new(keys::Key::fresh().unwrap()),
                attestation: crate::attestation::Attestation::new(
                    crate::attestation::Mode::Development,
                )
                .unwrap(),
                takes_over: Default::default(),
                api_url: Default::default(),
                holds: Default::default(),
                key: stand_in::key(&client),
                dry: Default::default(),
                metered: Default::default(),
                running: Default::default(),
                counters: Default::default(),
                watched_to: Default::default(),
                account_events: Default::default(),
                vault_events: Default::default(),
                wound_down: Default::default(),
                signed_through: signed_through.clone(),
                approvals: Default::default(),
                ceilings: Default::default(),
                failed_starts: Default::default(),
                halt_known: Default::default(),
            },
            serving: Some(tokio::spawn(async {
                Ended {
                    failed: false,
                    replaced: true,
                    resumed: false,
                }
            })),
            retry: Default::default(),
            _upkeep: [idle(), idle()],
            _leaf: no_leaf(),
            letting_go: None,
        };
        let mut book = Book::default();
        book.running.insert(vault, served);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The meter hasn't yet signed for the time up to the stop: the vault
        // is kept, and no new serve started.
        sweep(&mut book).await;
        let served = book
            .running
            .get(&vault)
            .expect("let go before its last bill was signed");
        assert!(served.serving.is_none(), "a new serve was started");

        // Once it has, the vault is let go.
        signed_through.signed_now();
        sweep(&mut book).await;
        assert!(!book.running.contains_key(&vault));

        // While the other key stands, the vault isn't taken on again.
        sweep(&mut book).await;
        sweep(&mut book).await;
        assert!(!book.running.contains_key(&vault));
        assert!(node.lock().unwrap().sent.is_empty());

        // Once its blessing lapses, it is.
        node.lock().unwrap().lapsed.insert(other);
        sweep(&mut book).await;
        assert!(book.running.contains_key(&vault), "not taken on again");
        drop(book);
        let _ = std::fs::remove_dir_all(&store);
    }

    /// A replaced executor stops reporting on the vault at once, so only the
    /// executor that holds it signs its reports, while its meter runs on for
    /// the last bill.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replaced_executor_stops_its_reports_and_keeps_its_meter() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let bundle = node.lock().unwrap().bundle;
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.approved_key = Address::repeat_byte(0x77);
        }
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let store =
            std::env::temp_dir().join(format!("executor-replaced-reports-{}", std::process::id()));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();

        /// A task that runs until it is stopped, and lowers its flag then.
        fn flagged() -> (AbortOnDrop, Arc<AtomicBool>) {
            struct Lowers(Arc<AtomicBool>);
            impl Drop for Lowers {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let flag = Arc::new(AtomicBool::new(true));
            let lowers = Lowers(flag.clone());
            let task = tokio::spawn(async move {
                let _lowers = lowers;
                std::future::pending::<()>().await
            });
            (AbortOnDrop(task), flag)
        }
        let (metering, meter_runs) = flagged();
        let (reporting, reports_run) = flagged();
        let served = Served {
            shared: Shared {
                vault_key: Arc::new(keys::Key::fresh().unwrap()),
                attestation: crate::attestation::Attestation::new(
                    crate::attestation::Mode::Development,
                )
                .unwrap(),
                takes_over: Default::default(),
                api_url: Default::default(),
                holds: Default::default(),
                key: stand_in::key(&client),
                dry: Default::default(),
                metered: Default::default(),
                running: Default::default(),
                counters: Default::default(),
                watched_to: Default::default(),
                account_events: Default::default(),
                vault_events: Default::default(),
                wound_down: Default::default(),
                signed_through: Default::default(),
                approvals: Default::default(),
                ceilings: Default::default(),
                failed_starts: Default::default(),
                halt_known: Default::default(),
            },
            serving: Some(tokio::spawn(async {
                Ended {
                    failed: false,
                    replaced: true,
                    resumed: false,
                }
            })),
            retry: Default::default(),
            _upkeep: [metering, reporting],
            _leaf: no_leaf(),
            letting_go: None,
        };
        let mut book = Book::default();
        book.running.insert(vault, served);
        tokio::time::sleep(Duration::from_millis(50)).await;

        sweep_vault(
            shared::Chain::Base,
            &client,
            &stand_in::deployments(),
            None,
            &runner::engine().unwrap(),
            &args(&store),
            &Default::default(),
            &mut book,
            vault,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = std::fs::remove_dir_all(&store);
        assert!(
            book.running.contains_key(&vault),
            "let go before its last bill was signed"
        );
        assert!(
            !reports_run.load(Ordering::SeqCst),
            "a replaced executor still reports on the vault"
        );
        assert!(
            meter_runs.load(Ordering::SeqCst),
            "the meter stopped before the last bill was signed"
        );
    }

    /// A status port that can't be bound is an error the executor stops on.
    #[tokio::test]
    async fn a_status_port_that_cannot_be_bound_is_an_error() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = taken.local_addr().unwrap();
        let bound = bind_status(address).await;
        drop(taken);
        assert!(bound.is_err(), "a port another socket holds was bound");
    }

    /// Whether a stale look at the station's approval can take it back.
    mod handover {
        use super::*;

        use alloy::consensus::{Transaction, TxEnvelope};
        use alloy::eips::eip2718::Decodable2718;
        use alloy::primitives::U256;
        use alloy::sol_types::SolCall;
        use axum::extract::State;
        use axum::response::Response;
        use axum::{Json, Router, routing::post};
        use chain::bindings::attestation::{ImageKind, KeyInfo};
        use serde_json::{Value, json};

        const NOW: u64 = 1_700_000_000;
        const TTL: u64 = 72 * 3_600;

        /// A station, registry and venue in which every key is blessed. This key's
        /// renewal has landed and the venue still holds it until the old blessing's
        /// end, so the first look finds the approval behind. While that look is being
        /// made, a successor's key takes the station's approval over, as `approveKey`
        /// from that key allows while ours is blessed.
        struct Handover {
            ours: Address,
            successor: Address,
            approved: Address,
            /// What the venue holds our key approved until.
            our_venue_until: u64,
            /// Whether the successor has taken over yet.
            taken_over: bool,
            /// Each approval sent: the key it approves, and which key held the
            /// station's approval when it arrived.
            approvals: Vec<(Address, Address)>,
            hashes: Vec<B256>,
        }

        type Node = Arc<Mutex<Handover>>;

        fn handover_answer(node: &mut Handover, input: &[u8]) -> Option<Vec<u8>> {
            let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
            let word = |value: u64| U256::from(value).to_be_bytes::<32>().to_vec();
            Some(match selector {
                IStation::approvedKeyCall::SELECTOR => {
                    IStation::approvedKeyCall::abi_encode_returns(&node.approved)
                }
                IAttestationRegistry::isBlessedForCall::SELECTOR => {
                    IAttestationRegistry::isBlessedForCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: B256::ZERO,
                        projectId: B256::ZERO,
                        kind: ImageKind::Executor,
                        strategyHash: B256::repeat_byte(0x42),
                        vaultChain: U256::from(31337u64),
                        vault: Address::repeat_byte(0x11),
                        trustRoot: B256::ZERO,
                        verifier: Address::ZERO,
                        blessedUntil: NOW + TTL,
                    })
                }
                IAttestationRegistry::blessedUntilCall::SELECTOR => word(NOW + TTL),
                ILocalVenue::agentValidUntilCall::SELECTOR => {
                    let until = node.our_venue_until;
                    // The successor's approval lands just after this look read the
                    // station: the look's answer is already stale.
                    if !node.taken_over {
                        node.taken_over = true;
                        node.approved = node.successor;
                        node.our_venue_until = 0;
                    }
                    word(until)
                }
                _ => return None,
            })
        }

        async fn handover_rpc(State(node): State<Node>, Json(request): Json<Value>) -> Response {
            use axum::response::IntoResponse;
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let mut node = node.lock().unwrap();
            let result = match method.as_str() {
                "eth_chainId" => json!("0x7a69"),
                "eth_accounts" => json!([]),
                "eth_blockNumber" => json!("0x10"),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.timestamp = NOW;
                    block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                    serde_json::to_value(block).unwrap()
                }
                "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
                "eth_feeHistory" => json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]],
                }),
                "eth_estimateGas" => json!("0x30000"),
                "eth_getTransactionCount" => json!(format!("0x{:x}", node.hashes.len())),
                "eth_getBalance" => json!(U256::from(10u128.pow(19))),
                "eth_sendRawTransaction" => {
                    let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                    let envelope = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                    let hash = *envelope.tx_hash();
                    if let Ok(call) = IStation::approveKeyCall::abi_decode(envelope.input()) {
                        let held = node.approved;
                        node.approvals.push((call.key, held));
                        node.approved = call.key;
                        if call.key == node.ours {
                            node.our_venue_until = NOW + TTL;
                        }
                    }
                    node.hashes.push(hash);
                    json!(hash)
                }
                "eth_getTransactionReceipt" => {
                    let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                    if node.hashes.contains(&asked) {
                        json!({
                            "transactionHash": asked,
                            "transactionIndex": "0x0",
                            "blockHash": B256::repeat_byte(0xbb),
                            "blockNumber": "0x11",
                            "from": Address::ZERO,
                            "to": Address::repeat_byte(0x22),
                            "cumulativeGasUsed": "0x5208",
                            "gasUsed": "0x5208",
                            "effectiveGasPrice": "0x3b9aca00",
                            "contractAddress": null,
                            "logs": [],
                            "logsBloom": format!("0x{}", "00".repeat(256)),
                            "status": "0x1",
                            "type": "0x2",
                        })
                    } else {
                        Value::Null
                    }
                }
                "eth_call" => {
                    let tx = &request["params"][0];
                    let data = tx["input"]
                        .as_str()
                        .or_else(|| tx["data"].as_str())
                        .unwrap();
                    let input = alloy::hex::decode(data).unwrap();
                    match handover_answer(&mut node, &input) {
                        Some(out) => json!(alloy::hex::encode_prefixed(out)),
                        None => {
                            return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}}))
                                .into_response();
                        }
                    }
                }
                _ => {
                    return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}))
                        .into_response();
                }
            };
            Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
        }

        /// One executor at a time. Our key's renewal lands and the follow looks: the
        /// venue holds our key until the old blessing's end, so the approval is
        /// behind. Between that look and the approval it would send, another
        /// executor's key takes the station over. `Station.approveKey` lets a
        /// still-blessed key replace another still-blessed one, so an approval sent on
        /// the stale look would take the vault back from the successor. Where the
        /// approval stands is read again immediately before every send, the first
        /// included, and nothing is sent once another key holds it.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_stale_look_does_not_take_the_approval_back_from_a_successor() {
            let placeholder = Address::ZERO;
            let node: Node = Arc::new(Mutex::new(Handover {
                ours: placeholder,
                successor: Address::repeat_byte(0x77),
                approved: placeholder,
                our_venue_until: NOW + TTL / 4,
                taken_over: false,
                approvals: Vec::new(),
                hashes: Vec::new(),
            }));
            let app = Router::new()
                .route("/", post(handover_rpc))
                .with_state(node.clone());
            let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = stand_in::client(&url).await;
            let key = stand_in::key(&client);
            {
                let mut node = node.lock().unwrap();
                node.ours = key.address;
                node.approved = key.address;
            }

            let renewed = Arc::new(tokio::sync::Notify::new());
            let replaced = Arc::new(AtomicBool::new(false));
            let following = tokio::spawn(follow_blessing(
                client,
                stand_in::deployments(),
                crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                    .unwrap(),
                key.clone(),
                Address::repeat_byte(0x55),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                Default::default(),
                renewed.clone(),
                replaced.clone(),
            ));
            renewed.notify_one();
            tokio::time::sleep(FOLLOW_EVERY * 4).await;
            following.abort();

            let node = node.lock().unwrap();
            let taken_back: Vec<_> = node
                .approvals
                .iter()
                .filter(|(approves, held)| *approves == node.ours && *held == node.successor)
                .collect();
            assert!(
                taken_back.is_empty(),
                "our key sent approveKey while the successor's still-blessed key held the station, \
                 taking the vault back on a stale look ({} times); now approved: {}",
                taken_back.len(),
                node.approved
            );
            assert!(
                replaced.load(Ordering::SeqCst),
                "the follow never saw that it had been replaced"
            );
        }
    }

    /// An approval whose send went unanswered (the node took it and never
    /// mined it while the sender waited) may still land for as long as its
    /// receipt is watched for. Another is not sent meanwhile, which could pay
    /// for two; where the approval stands is looked at, and once the first
    /// lands the follow is done.
    #[tokio::test]
    async fn an_approval_that_may_still_land_is_not_sent_again() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.held_approvals = true;
        }
        let pending = || node.lock().unwrap().pending.len();
        let following = {
            let client = client.clone();
            let key = key.clone();
            tokio::spawn(async move {
                approve_until_it_lands(
                    &client,
                    &stand_in::deployments(),
                    &crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                        .unwrap(),
                    &key,
                    station,
                    Address::repeat_byte(0x11),
                    B256::repeat_byte(0x42),
                    &Default::default(),
                )
                .await
            })
        };
        for _ in 0..200 {
            if pending() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(pending(), 1, "the approval was never sent");

        // Nothing is mined: the clock runs on past the sender's wait, and a
        // few minutes beyond it, inside the receipt's watch.
        tokio::time::pause();
        tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        tokio::time::resume();
        assert_eq!(
            pending(),
            1,
            "an approval was sent again while the first may still land"
        );

        // It lands.
        stand_in::serving::mine(&node);
        let held = tokio::time::timeout(Duration::from_secs(30), following)
            .await
            .expect("the landed approval was never seen")
            .unwrap();
        assert_eq!(held, Held::Kept);
        assert_eq!(stand_in::serving::approvals(&node), 1);
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
    }

    /// A serve that fails on an approval whose send went unanswered after it
    /// was broadcast is started again by the sweep after a few seconds, well
    /// inside the time that approval may still land. The new serve sends no
    /// approval of its own meanwhile, which would land too and be paid for
    /// twice: it looks at where the approval stands, and once the first one
    /// lands it carries on with nothing more sent.
    #[tokio::test]
    async fn a_serve_started_again_does_not_resend_an_approval_that_may_still_land() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            // Blessed for a while (no blessing is sent), no key approved yet,
            // and the node holds approvals without mining them.
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.held_approvals = true;
        }
        // What the vault's serves share, as the sweep keeps it.
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key,
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        let pending = || node.lock().unwrap().pending.len();
        let serve_once = || {
            let client = client.clone();
            let shared = shared.clone();
            tokio::spawn(async move {
                serve(
                    client,
                    shared::Chain::Base,
                    stand_in::deployments(),
                    runner::engine().unwrap(),
                    Address::repeat_byte(0x11),
                    B256::repeat_byte(0x42),
                    std::env::temp_dir(),
                    "local".to_string(),
                    shared,
                )
                .await
                .map(|_| ())
            })
        };

        // The first serve sends the approval, which the node takes and holds.
        let first = serve_once();
        for _ in 0..200 {
            if pending() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(pending(), 1, "the approval was never sent");

        // Nothing is mined; the serve gives up waiting on it and fails.
        tokio::time::pause();
        let failed = first.await.unwrap();
        tokio::time::resume();
        let error = failed.expect_err("the serve fails on the unanswered approval");
        assert!(
            chain::may_still_land(&error),
            "the approval was broadcast, so it may still land: {error:#}"
        );

        // The sweep starts the vault's serve again after its back-off.
        let second = serve_once();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            pending(),
            1,
            "a second approval was sent while the first may still land"
        );

        // The first lands; the serve sees it and sends nothing more.
        stand_in::serving::mine(&node);
        for _ in 0..200 {
            if node.lock().unwrap().venue_until == NOW + TTL {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        second.abort();
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
        assert_eq!(
            stand_in::serving::approvals(&node),
            1,
            "an approval was sent after the first had landed"
        );
    }

    /// A gateway in front of the serving stand-in that turns away the first
    /// `eth_estimateGas` it is asked with a 503, as a busy gateway does, and
    /// forwards everything else, noting each method that reached the node.
    #[derive(Clone)]
    struct BusyEstimate {
        upstream: String,
        http: reqwest::Client,
        estimates_to_fail: Arc<std::sync::atomic::AtomicU32>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    async fn busy_estimate(
        axum::extract::State(gateway): axum::extract::State<BusyEstimate>,
        axum::Json(request): axum::Json<serde_json::Value>,
    ) -> axum::response::Response {
        let method = request["method"].as_str().unwrap_or_default().to_string();
        if method == "eth_estimateGas"
            && gateway
                .estimates_to_fail
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
        }
        gateway.seen.lock().unwrap().push(method);
        let answer: serde_json::Value = gateway
            .http
            .post(&gateway.upstream)
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        axum::Json(answer).into_response()
    }

    /// An approval that failed before it was broadcast (its gas estimate
    /// turned away) can't land, so it doesn't hold the next one back for the
    /// time a sent one is watched for: it is tried again after the usual
    /// wait, and lands.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_approval_never_broadcast_is_tried_again_without_waiting_for_it_to_land() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let upstream = stand_in::serving::start(node.clone()).await;
        let gateway = BusyEstimate {
            upstream,
            http: reqwest::Client::new(),
            estimates_to_fail: Arc::new(std::sync::atomic::AtomicU32::new(1)),
            seen: Default::default(),
        };
        let app = axum::Router::new()
            .route("/", axum::routing::post(busy_estimate))
            .with_state(gateway.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = stand_in::client(&url).await;
        let key = stand_in::key(&client);
        {
            // Renewed: the venue still holds the key approved only to the
            // old blessing's end.
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.venue_until = NOW + TTL / 2;
            node.approved_key = key.address;
        }

        let held = tokio::time::timeout(
            Duration::from_secs(10),
            approve_until_it_lands(
                &client,
                &stand_in::deployments(),
                &crate::attestation::Attestation::new(crate::attestation::Mode::Development)
                    .unwrap(),
                &key,
                station,
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                &Default::default(),
            ),
        )
        .await
        .expect("an approval that was never broadcast was waited on as if it might land");
        assert_eq!(held, Held::Kept);
        assert_eq!(node.lock().unwrap().venue_until, NOW + TTL);
        let broadcasts = gateway
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "eth_sendRawTransaction")
            .count();
        assert_eq!(broadcasts, 1, "only the approval that landed was broadcast");
    }

    /// A station that refuses to approve the key (its driver revoked) ends
    /// the follow as being replaced does, so the serve stops the agent, which
    /// can't trade, and its billing; the approval isn't sent again and again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_station_that_refuses_the_key_stops_the_serve_as_a_replacement_does() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let station = node.lock().unwrap().station;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        {
            let mut node = node.lock().unwrap();
            node.approved_key = key.address;
            node.blessed_until = NOW + TTL;
            node.venue_until = NOW + TTL / 4;
            node.driver_revoked = true;
        }
        let replaced = Arc::new(AtomicBool::new(false));
        let following = tokio::spawn(follow_blessing(
            client.clone(),
            stand_in::deployments(),
            crate::attestation::Attestation::new(crate::attestation::Mode::Development).unwrap(),
            key.clone(),
            station,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x42),
            Default::default(),
            Default::default(),
            replaced.clone(),
        ));
        tokio::time::sleep(FOLLOW_EVERY * 5).await;
        following.abort();
        assert!(
            replaced.load(Ordering::SeqCst),
            "the serve was not stopped though the station refuses the key"
        );
        assert_eq!(stand_in::serving::approvals(&node), 1);

        // At a serve's start, the same.
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Default::default(),
            api_url: Default::default(),
            holds: Default::default(),
            key,
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        let served = tokio::time::timeout(
            Duration::from_secs(30),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                std::env::temp_dir(),
                "local".to_string(),
                shared,
            ),
        )
        .await
        .expect("the serve carried on");
        assert_eq!(served.unwrap(), Stop::Replaced);
        assert_eq!(
            node.lock().unwrap().agent_starts,
            0,
            "the agent was started"
        );
    }

    /// A vault let go because the station refuses its key is not taken on
    /// again (a stake and a blessing paid for nothing) while the station
    /// would still refuse it, and is once it would approve it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vault_whose_key_was_refused_is_taken_on_again_once_it_can_be_approved() {
        use stand_in::serving::{NOW, TTL};
        let node = stand_in::serving::node(VaultState::Open);
        let bundle = node.lock().unwrap().bundle;
        {
            let mut node = node.lock().unwrap();
            node.blessed_until = NOW + TTL;
            node.driver_revoked = true;
        }
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let store = std::env::temp_dir().join(format!("executor-refused-{}", std::process::id()));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();
        let sweep = async |book: &mut Book| {
            sweep_vault(
                shared::Chain::Base,
                &client,
                &stand_in::deployments(),
                None,
                &runner::engine().unwrap(),
                &args(&store),
                &Default::default(),
                book,
                vault,
            )
            .await
            .unwrap();
        };
        let mut book = Book::default();
        book.replaced.insert(vault);

        sweep(&mut book).await;
        sweep(&mut book).await;
        assert!(
            !book.running.contains_key(&vault),
            "taken on though refused"
        );
        assert!(node.lock().unwrap().sent.is_empty());

        node.lock().unwrap().driver_revoked = false;
        sweep(&mut book).await;
        assert!(book.running.contains_key(&vault), "not taken on again");
        drop(book);
        let _ = std::fs::remove_dir_all(&store);
    }

    /// An approval and the key's turn to send.
    mod turn {
        use super::*;

        use alloy::consensus::{Transaction, TxEnvelope};
        use alloy::eips::eip2718::Decodable2718;
        use alloy::primitives::U256;
        use alloy::sol_types::SolCall;
        use axum::extract::State;
        use axum::response::Response;
        use axum::{Json, Router, routing::post};
        use chain::bindings::attestation::{ImageKind, KeyInfo};
        use serde_json::{Value, json};

        pub(super) const NOW: u64 = 1_700_000_000;
        const TTL: u64 = 72 * 3_600;

        /// A station, registry and venue in which every key is blessed. Our key's
        /// renewal has landed and the venue still holds it until the old blessing's
        /// end, so a look finds the approval behind. The agent has a transaction in
        /// flight whose receipt the node withholds until the test releases it.
        struct Node {
            ours: Address,
            successor: Address,
            approved: Address,
            our_venue_until: u64,
            /// How many times the venue's approval of our key was read.
            venue_reads: usize,
            /// Whether the agent's transaction's receipt is withheld.
            withheld: bool,
            /// The agent's transaction.
            blocker: Option<B256>,
            hashes: Vec<B256>,
            /// Each approval sent: the key it approves, and which key held the
            /// station's approval when it arrived.
            approvals: Vec<(Address, Address)>,
        }

        type Shared = Arc<Mutex<Node>>;

        fn answer_call(node: &mut Node, input: &[u8]) -> Option<Vec<u8>> {
            let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
            let word = |value: u64| U256::from(value).to_be_bytes::<32>().to_vec();
            Some(match selector {
                IStation::approvedKeyCall::SELECTOR => {
                    IStation::approvedKeyCall::abi_encode_returns(&node.approved)
                }
                IAttestationRegistry::isBlessedForCall::SELECTOR => {
                    IAttestationRegistry::isBlessedForCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: B256::ZERO,
                        projectId: B256::ZERO,
                        kind: ImageKind::Executor,
                        strategyHash: B256::repeat_byte(0x42),
                        vaultChain: U256::from(31337u64),
                        vault: Address::repeat_byte(0x11),
                        trustRoot: B256::ZERO,
                        verifier: Address::ZERO,
                        blessedUntil: NOW + TTL,
                    })
                }
                IAttestationRegistry::blessedUntilCall::SELECTOR => word(NOW + TTL),
                ILocalVenue::agentValidUntilCall::SELECTOR => {
                    node.venue_reads += 1;
                    word(node.our_venue_until)
                }
                _ => return None,
            })
        }

        pub(super) fn receipt(hash: B256) -> Value {
            json!({
                "transactionHash": hash,
                "transactionIndex": "0x0",
                "blockHash": B256::repeat_byte(0xbb),
                "blockNumber": "0x11",
                "from": Address::ZERO,
                "to": Address::repeat_byte(0x22),
                "cumulativeGasUsed": "0x5208",
                "gasUsed": "0x5208",
                "effectiveGasPrice": "0x3b9aca00",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "00".repeat(256)),
                "status": "0x1",
                "type": "0x2",
            })
        }

        async fn rpc(State(node): State<Shared>, Json(request): Json<Value>) -> Response {
            use axum::response::IntoResponse;
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let mut node = node.lock().unwrap();
            let result = match method.as_str() {
                "eth_chainId" => json!("0x7a69"),
                "eth_accounts" => json!([]),
                "eth_blockNumber" => json!("0x10"),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.timestamp = NOW;
                    block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                    serde_json::to_value(block).unwrap()
                }
                "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
                "eth_feeHistory" => json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]],
                }),
                "eth_estimateGas" => json!("0x30000"),
                "eth_getTransactionCount" => json!(format!("0x{:x}", node.hashes.len())),
                "eth_getBalance" => json!(U256::from(10u128.pow(19))),
                "eth_sendRawTransaction" => {
                    let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                    let envelope = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                    let hash = *envelope.tx_hash();
                    if let Ok(call) = IStation::approveKeyCall::abi_decode(envelope.input()) {
                        let held = node.approved;
                        node.approvals.push((call.key, held));
                        node.approved = call.key;
                        if call.key == node.ours {
                            node.our_venue_until = NOW + TTL;
                        }
                    } else {
                        node.blocker = Some(hash);
                    }
                    node.hashes.push(hash);
                    json!(hash)
                }
                "eth_getTransactionReceipt" => {
                    let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                    if node.withheld && node.blocker == Some(asked) {
                        Value::Null
                    } else if node.hashes.contains(&asked) {
                        receipt(asked)
                    } else {
                        Value::Null
                    }
                }
                "eth_call" => {
                    let tx = &request["params"][0];
                    let data = tx["input"]
                        .as_str()
                        .or_else(|| tx["data"].as_str())
                        .unwrap();
                    let input = alloy::hex::decode(data).unwrap();
                    match answer_call(&mut node, &input) {
                        Some(out) => json!(alloy::hex::encode_prefixed(out)),
                        None => {
                            return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}}))
                                .into_response();
                        }
                    }
                }
                _ => {
                    return Json(
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}),
                    )
                    .into_response();
                }
            };
            Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
        }

        pub(super) async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
            for _ in 0..200 {
                if done() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("never happened: {what}");
        }

        /// One executor at a time. An approval that waits for the key's turn to
        /// send (one of the agent's orders holds it) reads where the approval stands
        /// once the turn has come, not before: a successor's key may take the
        /// station over meanwhile, and `Station.approveKey` would let an approval
        /// sent on the earlier look take the vault back from it.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_approval_waiting_for_the_key_s_turn_does_not_take_the_vault_back() {
            let node: Shared = Arc::new(Mutex::new(Node {
                ours: Address::ZERO,
                successor: Address::repeat_byte(0x77),
                approved: Address::ZERO,
                our_venue_until: NOW + TTL / 4,
                venue_reads: 0,
                withheld: true,
                blocker: None,
                hashes: Vec::new(),
                approvals: Vec::new(),
            }));
            let app = Router::new().route("/", post(rpc)).with_state(node.clone());
            let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = stand_in::client(&url).await;
            let key = stand_in::key(&client);
            {
                let mut node = node.lock().unwrap();
                node.ours = key.address;
                node.approved = key.address;
            }
            let station = Address::repeat_byte(0x55);
            let vault = Address::repeat_byte(0x11);
            let bundle = B256::repeat_byte(0x42);

            // The agent places an order: its transaction holds the key's turn until
            // its receipt comes.
            let agent = key.clone();
            let order = tokio::spawn(async move {
                agent
                    .send(
                        Address::repeat_byte(0x33),
                        ILocalVenue::placeCall {
                            master: station,
                            orders: vec![],
                        },
                    )
                    .await
            });
            wait_for("the agent's order to be sent", || {
                node.lock().unwrap().blocker.is_some()
            })
            .await;

            // A renewal has landed: the approval is behind, and is followed.
            let platform = key.unmetered();
            let following = {
                let client = client.clone();
                tokio::spawn(async move {
                    approve_until_it_lands(
                        &client,
                        &stand_in::deployments(),
                        &crate::attestation::Attestation::new(
                            crate::attestation::Mode::Development,
                        )
                        .unwrap(),
                        &platform,
                        station,
                        vault,
                        bundle,
                        &Default::default(),
                    )
                    .await
                })
            };
            wait_for("the look at the approval", || {
                node.lock().unwrap().venue_reads >= 1
            })
            .await;
            // The look found it behind; the approval now waits for the key's turn.
            tokio::time::sleep(Duration::from_millis(300)).await;

            // Meanwhile a successor's key takes the station over, and the agent's
            // order lands, giving the turn up.
            {
                let mut node = node.lock().unwrap();
                node.approved = node.successor;
                node.our_venue_until = 0;
                node.withheld = false;
            }
            let _ = tokio::time::timeout(Duration::from_secs(30), order).await;
            let held = tokio::time::timeout(Duration::from_secs(30), following)
                .await
                .expect("the follow never finished")
                .unwrap();

            let node = node.lock().unwrap();
            let taken_back: Vec<_> = node
                .approvals
                .iter()
                .filter(|(approves, held)| *approves == node.ours && *held == node.successor)
                .collect();
            assert!(
                taken_back.is_empty(),
                "our key's approveKey, sent on a look taken before it waited for the key's turn, \
                 took the station back from the successor ({} times); now approved: {}",
                taken_back.len(),
                node.approved
            );
            assert_eq!(
                held,
                Held::Replaced,
                "the follow did not see it was replaced"
            );
        }
    }

    /// The let-go's last bill and the agent's transactions still landing.
    mod let_go {
        use super::*;

        use alloy::consensus::TxEnvelope;
        use alloy::eips::eip2718::Decodable2718;
        use alloy::primitives::U256;
        use alloy::sol_types::SolCall;
        use axum::extract::State;
        use axum::response::Response;
        use axum::{Json, Router, routing::post};
        use serde_json::{Value, json};

        use super::turn::{NOW, receipt, wait_for};
        use chain::bindings::vault::{IVault, Tank, VaultState};

        struct Node {
            /// The agent's order, whose receipt is withheld until released.
            order: Option<B256>,
            withheld: bool,
            /// Whether the vault reads as closed.
            closed: bool,
        }

        type Shared = Arc<Mutex<Node>>;

        fn answer_call(node: &Node, input: &[u8]) -> Option<Vec<u8>> {
            let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
            Some(match selector {
                IVault::stateCall::SELECTOR => {
                    IVault::stateCall::abi_encode_returns(if node.closed {
                        &VaultState::Closed
                    } else {
                        &VaultState::Open
                    })
                }
                IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&Tank {
                    balance: U256::from(1_000_000_000_000u64),
                    keepAt: U256::ZERO,
                    usageSettled: U256::ZERO,
                    usageAsOf: 0,
                    lastCollection: 0,
                    escapeRequestedAt: 0,
                    capCarried: U256::ZERO,
                    fillPending: false,
                }),
                // Gas only, and a generous allowance of it: the bill is the gas
                // the key actually paid for.
                IVault::allowancesCall::SELECTOR => IVault::allowancesCall::abi_encode_returns(
                    &chain::allowances(&shared::Allowances {
                        fuel_per_sec: 0,
                        memory_mib: 0,
                        requests_per_min: 0,
                        bytes_per_day: 0,
                        streams: 0,
                        inference_ms_per_min: 0,
                        gas_per_day: 1_000_000.0,
                    }),
                ),
                IVault::usageDigestCall::SELECTOR => IVault::usageDigestCall::abi_encode_returns(
                    &alloy::primitives::keccak256(input),
                ),
                _ => return None,
            })
        }

        async fn rpc(State(node): State<Shared>, Json(request): Json<Value>) -> Response {
            use axum::response::IntoResponse;
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let mut node = node.lock().unwrap();
            let result = match method.as_str() {
                "eth_chainId" => json!("0x7a69"),
                "eth_accounts" => json!([]),
                "eth_blockNumber" => json!("0x10"),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.timestamp = NOW;
                    block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                    serde_json::to_value(block).unwrap()
                }
                "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
                "eth_feeHistory" => json!({
                    "oldestBlock": "0x1",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]],
                }),
                "eth_estimateGas" => json!("0x30000"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getBalance" => json!(U256::from(10u128.pow(19))),
                "eth_sendRawTransaction" => {
                    let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                    let envelope = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                    let hash = *envelope.tx_hash();
                    node.order = Some(hash);
                    json!(hash)
                }
                "eth_getTransactionReceipt" => {
                    let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                    if !node.withheld && node.order == Some(asked) {
                        receipt(asked)
                    } else {
                        Value::Null
                    }
                }
                "eth_call" => {
                    let tx = &request["params"][0];
                    let data = tx["input"]
                        .as_str()
                        .or_else(|| tx["data"].as_str())
                        .unwrap();
                    let input = alloy::hex::decode(data).unwrap();
                    match answer_call(&node, &input) {
                        Some(out) => json!(alloy::hex::encode_prefixed(out)),
                        None => {
                            return Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": "execution reverted"}}))
                                .into_response();
                        }
                    }
                }
                _ => {
                    return Json(
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}),
                    )
                    .into_response();
                }
            };
            Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
        }

        /// A replaced executor's agent stops with an order of its own in flight
        /// (the serve is ended mid-send; the send's watch carries on, as
        /// `KeyClient::send` means it to, and counts its gas when the receipt
        /// comes). The vault, and its meter with it, is kept until that order has
        /// landed and a total counting its gas is signed, so the agent's last
        /// orders are billed.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_vault_let_go_bills_the_gas_of_the_agent_s_last_orders() {
            the_agent_s_last_orders_are_billed(false).await;
        }

        /// The same for a vault that closes (the keeper's Closer, say) while
        /// its agent's last order is still landing: the meter is kept until
        /// that order's gas is in a signed total, which the keeper collects in
        /// the closed vault's last bill.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_vault_that_closes_bills_the_gas_of_the_agent_s_last_orders() {
            the_agent_s_last_orders_are_billed(true).await;
        }

        async fn the_agent_s_last_orders_are_billed(closes: bool) {
            let node: Shared = Arc::new(Mutex::new(Node {
                order: None,
                withheld: true,
                closed: false,
            }));
            let app = Router::new().route("/", post(rpc)).with_state(node.clone());
            let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = stand_in::client(&url).await;
            let key = stand_in::key(&client);
            let vault = Address::repeat_byte(0x11);
            let totals: usage::Totals = Default::default();
            let tank = usage::Tank::default();
            let signed_through = tank.signed_through.clone();
            let metering = tokio::spawn(usage::run(
                client.clone(),
                key.clone(),
                vault,
                Some(2_500.0),
                totals.clone(),
                tank,
                Arc::new(AtomicBool::new(false)),
                Default::default(),
                0,
            ));

            // The agent's last order goes out, and the serve ends (another
            // executor has taken the vault over) or the vault closes.
            let agent = key.clone();
            let order = tokio::spawn(async move {
                agent
                    .send(
                        Address::repeat_byte(0x33),
                        ILocalVenue::placeCall {
                            master: Address::repeat_byte(0x55),
                            orders: vec![],
                        },
                    )
                    .await
            });
            wait_for("the order to be sent", || {
                node.lock().unwrap().order.is_some()
            })
            .await;
            order.abort();

            let idle = || AbortOnDrop(tokio::spawn(std::future::pending()));
            let served = Served {
                shared: crate::Shared {
                    vault_key: Arc::new(keys::Key::fresh().unwrap()),
                    attestation: crate::attestation::Attestation::new(
                        crate::attestation::Mode::Development,
                    )
                    .unwrap(),
                    takes_over: Default::default(),
                    api_url: Default::default(),
                    holds: Default::default(),
                    key: key.clone(),
                    dry: Default::default(),
                    metered: Default::default(),
                    running: Default::default(),
                    counters: Default::default(),
                    watched_to: Default::default(),
                    account_events: Default::default(),
                    vault_events: Default::default(),
                    wound_down: Default::default(),
                    signed_through,
                    approvals: Default::default(),
                    ceilings: Default::default(),
                    failed_starts: Default::default(),
                    halt_known: Default::default(),
                },
                serving: Some(tokio::spawn(async move {
                    if closes {
                        // The agent runs on until it is stopped.
                        std::future::pending::<()>().await;
                    }
                    Ended {
                        failed: false,
                        replaced: true,
                        resumed: false,
                    }
                })),
                retry: Default::default(),
                _upkeep: [AbortOnDrop(metering), idle()],
                _leaf: no_leaf(),
                letting_go: None,
            };
            let mut book = Book::default();
            book.running.insert(vault, served);
            node.lock().unwrap().closed = closes;
            tokio::time::sleep(Duration::from_millis(50)).await;

            let args =
                args(&std::env::temp_dir().join(format!("executor-let-go-{}", std::process::id())));
            let engine = runner::engine().unwrap();
            let sweep = async |book: &mut Book| {
                sweep_vault(
                    shared::Chain::Base,
                    &client,
                    &stand_in::deployments(),
                    Some(2_500.0),
                    &engine,
                    &args,
                    &totals,
                    book,
                    vault,
                )
                .await
                .unwrap();
            };

            // Swept for a while, several totals signed meanwhile: the order is
            // still landing, so the vault is kept.
            for _ in 0..20 {
                sweep(&mut book).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(
                book.running.contains_key(&vault),
                "let go while the agent's last order was still landing"
            );
            let served = &book.running[&vault];
            assert!(served.serving.is_none(), "the agent was not stopped");

            // The order lands, as orders do, and the vault is let go once a
            // total counting it is signed.
            node.lock().unwrap().withheld = false;
            for _ in 0..100 {
                sweep(&mut book).await;
                if !book.running.contains_key(&vault) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(!book.running.contains_key(&vault), "never let go");
            assert!(
                key.gas_spent() > 0,
                "the stand-in never gave the order's receipt"
            );
            // 21,000 gas at one gwei, at $2,500: five cents and a quarter.
            let paid = 52_500u64;
            let billed = stand_in::latest_total(&totals, vault).unwrap_or(0);
            assert!(
                billed >= paid,
                "the key paid {paid} for the agent's last order and the last total bills {billed}"
            );
            // A closed vault isn't taken for one another executor holds.
            assert_eq!(book.replaced.contains(&vault), !closes);
        }
    }

    /// Serve vault 0x11 against the serving stand-in with `key`, taking the
    /// vault over from `takes_over`, until the serve ends.
    async fn serve_taking_over(
        client: Client,
        key: chain::KeyClient,
        takes_over: Option<crate::handover::Takeover>,
        api: &str,
    ) -> Result<Stop> {
        serve_taking_over_holding(client, key, takes_over, api, Default::default()).await
    }

    /// The same, with the serve's hold on the station's approval kept in
    /// `holds`.
    async fn serve_taking_over_holding(
        client: Client,
        key: chain::KeyClient,
        takes_over: Option<crate::handover::Takeover>,
        api: &str,
        holds: health::Holds,
    ) -> Result<Stop> {
        let shared = Shared {
            vault_key: Arc::new(keys::Key::fresh().unwrap()),
            attestation: crate::attestation::Attestation::new(
                crate::attestation::Mode::Development,
            )
            .unwrap(),
            takes_over: Arc::new(Mutex::new(takes_over)),
            api_url: api.to_string(),
            holds,
            key,
            dry: Default::default(),
            metered: Default::default(),
            running: Default::default(),
            counters: Default::default(),
            watched_to: Default::default(),
            account_events: Default::default(),
            vault_events: Default::default(),
            wound_down: Default::default(),
            signed_through: Default::default(),
            approvals: Default::default(),
            ceilings: Default::default(),
            failed_starts: Default::default(),
            halt_known: Default::default(),
        };
        tokio::time::timeout(
            Duration::from_secs(60),
            serve(
                client,
                shared::Chain::Base,
                stand_in::deployments(),
                runner::engine().unwrap(),
                Address::repeat_byte(0x11),
                B256::repeat_byte(0x42),
                std::env::temp_dir(),
                "local".to_string(),
                shared,
            ),
        )
        .await
        .expect("the serve ended")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_takeover_approves_its_own_key_in_place_of_the_silent_key_and_of_no_other() {
        let silent = Address::repeat_byte(0xee);
        // The station approves the silent key: the takeover replaces it.
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = silent;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        // The API has heard nothing from it since.
        let (api, _api) = api_holding(None).await;
        let takeover = crate::handover::Takeover::new(silent, unix_now() - 60);
        let served = serve_taking_over(client, key.clone(), Some(takeover), &api).await;
        assert!(served.is_err(), "the stand-in reverts the strategy's id");
        assert_eq!(node.lock().unwrap().approved_key, key.address);
        assert_eq!(stand_in::serving::approvals(&node), 1);

        // Another key holds the approval by the time the serve starts: that
        // one is not the key whose silence was watched, so nothing is sent.
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = Address::repeat_byte(0xdd);
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        let served = serve_taking_over(client, key, Some(takeover), &api).await;
        assert_eq!(served.unwrap(), Stop::Replaced);
        assert!(node.lock().unwrap().sent.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_takeover_leaves_the_vault_to_the_key_it_watched_once_that_key_reports_again() {
        let watched = alloy::signers::local::PrivateKeySigner::random();
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = watched.address();
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        // The takeover was decided a while ago; while the new key waited for
        // its blessing, the watched key signed a report again.
        let decided = unix_now() - 1_200;
        let (api, _api) = api_holding(Some((&watched, unix_now() - 30))).await;
        let takeover = crate::handover::Takeover::new(watched.address(), decided);
        let served = serve_taking_over(client, key, Some(takeover), &api).await;
        assert_eq!(served.unwrap(), Stop::Resumed);
        assert_eq!(node.lock().unwrap().approved_key, watched.address());
        assert_eq!(stand_in::serving::approvals(&node), 0);

        // An API that can't be asked is no silence: nothing is approved.
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = watched.address();
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        let served = serve_taking_over(client, key, Some(takeover), "http://127.0.0.1:9").await;
        assert!(served.is_err());
        assert_eq!(node.lock().unwrap().approved_key, watched.address());
        assert_eq!(stand_in::serving::approvals(&node), 0);
    }

    /// An API that holds, for every vault, the given report signed by `by`
    /// (or none), and the URL it answers at.
    async fn api_holding(
        report: Option<(&alloy::signers::local::PrivateKeySigner, u64)>,
    ) -> (String, AbortOnDrop) {
        let post = report.map(|(by, at)| signed_report(by, at));
        let app = axum::Router::new().route(
            "/api/{chain}/health/{vault}/signed",
            axum::routing::get(move || {
                let post = post.clone();
                async move {
                    match post {
                        Some(post) => axum::Json(post).into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (
            url,
            AbortOnDrop(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            })),
        )
    }

    /// A report on vault 0x11 signed by `by`, dated `at`.
    fn signed_report(by: &alloy::signers::local::PrivateKeySigner, at: u64) -> shared::HealthPost {
        let report = shared::model::health::Health {
            signals: vec![],
            entries: vec![],
            updated: time::PrimitiveDateTime::new(
                time::Date::from_ordinal_date(2026, 280).unwrap(),
                time::Time::MIDNIGHT,
            ),
            signed_by: by.address().to_string(),
            at,
        };
        let vault = format!("{:?}", Address::repeat_byte(0x11));
        let digest = shared::HealthPost::digest(shared::Chain::Base, &vault, &report);
        use alloy::signers::SignerSync;
        let signature = by.sign_message_sync(&digest).unwrap();
        shared::HealthPost {
            vault,
            report,
            signature: alloy::hex::encode_prefixed(signature.as_bytes()),
            signer: None,
        }
    }

    /// An API that, as the indexer does, keeps the reports posted to it and
    /// answers a vault's newest; it starts out holding `held`. Says its URL
    /// and every report posted to it.
    async fn api_keeping_the_newest(
        held: Vec<shared::HealthPost>,
    ) -> (String, Arc<Mutex<Vec<shared::HealthPost>>>, AbortOnDrop) {
        let held = Arc::new(Mutex::new(held));
        let posted: Arc<Mutex<Vec<shared::HealthPost>>> = Default::default();
        let app = axum::Router::new()
            .route(
                "/api/{chain}/health",
                axum::routing::post({
                    let held = held.clone();
                    let posted = posted.clone();
                    move |axum::Json(post): axum::Json<shared::HealthPost>| async move {
                        posted.lock().unwrap().push(post.clone());
                        held.lock().unwrap().push(post);
                        axum::http::StatusCode::ACCEPTED
                    }
                }),
            )
            .route(
                "/api/{chain}/health/{vault}/signed",
                axum::routing::get(move || {
                    let newest = held
                        .lock()
                        .unwrap()
                        .iter()
                        .max_by_key(|post| post.report.at)
                        .cloned();
                    async move {
                        match newest {
                            Some(post) => axum::Json(post).into_response(),
                            None => axum::http::StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (
            url,
            posted,
            AbortOnDrop(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            })),
        )
    }

    /// The vault's key reports only once its serve holds the station's
    /// approval. A takeover's key, blessed and waiting for its gas, sends
    /// nothing, so a watched key heard from again during that wait is never
    /// hidden behind the waiting key's own newer reports: the vault is left
    /// to it. A takeover from a key that stays silent takes the approval,
    /// and its key reports from then on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_waiting_to_take_a_vault_over_sends_no_report_that_could_hide_the_watched_key_s()
    {
        let watched = alloy::signers::local::PrivateKeySigner::random();
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = watched.address();
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        // The takeover was decided 20 minutes ago; the watched key came back
        // and reported 30 s ago, while this key, blessed meanwhile, waited.
        let decided = unix_now() - 1_200;
        let (api, posted, _api) =
            api_keeping_the_newest(vec![signed_report(&watched, unix_now() - 30)]).await;
        let holds = health::Holds::default();
        let mut reporter = health::Reporter::new(
            client.clone(),
            shared::Chain::Base,
            stand_in::deployments().attestation,
            key.clone(),
            Address::repeat_byte(0x11),
            api.clone(),
            holds.clone(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        reporter.post(Default::default()).await;
        let takeover = crate::handover::Takeover::new(watched.address(), decided);
        let served =
            serve_taking_over_holding(client, key.clone(), Some(takeover), &api, holds.clone())
                .await;
        reporter.post(Default::default()).await;
        assert_eq!(served.unwrap(), Stop::Resumed);
        assert_eq!(stand_in::serving::approvals(&node), 0);
        assert_eq!(node.lock().unwrap().approved_key, watched.address());
        assert!(
            posted.lock().unwrap().is_empty(),
            "a key that never held the station's approval reported on the vault"
        );

        // The watched key stays silent: the takeover takes the approval, and
        // its key's reports go from then on.
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = watched.address();
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let key = stand_in::key(&client);
        let (api, posted, _api) =
            api_keeping_the_newest(vec![signed_report(&watched, decided - 600)]).await;
        let holds = health::Holds::default();
        let mut reporter = health::Reporter::new(
            client.clone(),
            shared::Chain::Base,
            stand_in::deployments().attestation,
            key.clone(),
            Address::repeat_byte(0x11),
            api.clone(),
            holds.clone(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let served =
            serve_taking_over_holding(client, key.clone(), Some(takeover), &api, holds.clone())
                .await;
        assert!(served.is_err(), "the stand-in reverts the strategy's id");
        assert_eq!(node.lock().unwrap().approved_key, key.address);
        assert!(holds.holds());
        reporter.post(Default::default()).await;
        let posted = posted.lock().unwrap();
        assert_eq!(
            posted.len(),
            1,
            "the key holding the approval sent no report"
        );
        assert_eq!(
            posted[0].report.signed_by.to_lowercase(),
            format!("{:?}", key.address).to_lowercase()
        );
    }

    /// One sweep of vault 0x11 against the serving stand-in, whose station
    /// approves `by`'s key (blessed), with the API at `api` and the given
    /// silence. Says the book after it.
    async fn sweep_held_by(by: Address, api: &str, silence: u64, name: &str) -> Book {
        let node = stand_in::serving::node(VaultState::Open);
        node.lock().unwrap().approved_key = by;
        let bundle = node.lock().unwrap().bundle;
        let client = stand_in::client(&stand_in::serving::start(node.clone()).await).await;
        let store =
            std::env::temp_dir().join(format!("executor-handover-{name}-{}", std::process::id()));
        let component = bundle_path(&store, bundle);
        std::fs::create_dir_all(component.parent().unwrap()).unwrap();
        std::fs::write(&component, b"").unwrap();
        let mut args = args(&store);
        args.api_url = api.to_string();
        args.handover_silence_seconds = silence;
        let mut book = Book::default();
        sweep_vault(
            shared::Chain::Base,
            &client,
            &stand_in::deployments(),
            None,
            &runner::engine().unwrap(),
            &args,
            &Default::default(),
            &mut book,
            Address::repeat_byte(0x11),
        )
        .await
        .unwrap();
        let _ = std::fs::remove_dir_all(&store);
        book
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vault_whose_approved_key_still_reports_is_left_to_its_executor() {
        let other = alloy::signers::local::PrivateKeySigner::random();
        let (api, _api) = api_holding(Some((&other, unix_now()))).await;
        // However short the silence, a report dated after the watch began
        // starts it again.
        let book = sweep_held_by(other.address(), &api, 0, "reporting").await;
        let vault = Address::repeat_byte(0x11);
        assert!(
            !book.running.contains_key(&vault),
            "a live executor's vault was taken over"
        );
        assert_eq!(book.watching[&vault].by, other.address());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vault_whose_approved_key_is_silent_for_the_watch_is_taken_over_from_that_key() {
        let other = alloy::signers::local::PrivateKeySigner::random();
        // The API has only an old report from that key, from before the watch.
        let (api, _api) = api_holding(Some((&other, unix_now() - 3_600))).await;
        let book = sweep_held_by(other.address(), &api, 0, "silent").await;
        let vault = Address::repeat_byte(0x11);
        let served = book
            .running
            .get(&vault)
            .expect("a silent executor's vault was not taken over");
        assert_eq!(
            served
                .shared
                .takes_over
                .lock()
                .unwrap()
                .map(|takeover| takeover.from),
            Some(other.address()),
            "the takeover does not name the silent key"
        );
        assert_ne!(served.shared.key.address, other.address());
        assert!(!book.watching.contains_key(&vault));

        // Nor is a report signed by another key taken for the approved key's.
        let stranger = alloy::signers::local::PrivateKeySigner::random();
        let (api, _api) = api_holding(Some((&stranger, unix_now()))).await;
        let book = sweep_held_by(other.address(), &api, 0, "stranger").await;
        assert!(book.running.contains_key(&vault));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_api_out_of_reach_is_never_taken_for_a_silent_executor() {
        let other = alloy::signers::local::PrivateKeySigner::random();
        let book = sweep_held_by(other.address(), "http://127.0.0.1:9", 0, "unreachable").await;
        let vault = Address::repeat_byte(0x11);
        assert!(!book.running.contains_key(&vault));
        assert!(book.watching.contains_key(&vault));
    }

    /// The silence is counted from when the watch began: a key with no report
    /// at all is not taken over before the silence has passed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_with_no_report_is_taken_over_only_once_the_silence_has_passed() {
        let other = alloy::signers::local::PrivateKeySigner::random();
        let (api, _api) = api_holding(None).await;
        let book = sweep_held_by(
            other.address(),
            &api,
            crate::handover::HANDOVER_SILENCE,
            "waiting",
        )
        .await;
        assert!(!book.running.contains_key(&Address::repeat_byte(0x11)));
        let book = sweep_held_by(other.address(), &api, 0, "past").await;
        assert!(book.running.contains_key(&Address::repeat_byte(0x11)));
    }
}

#[cfg(test)]
mod key_driver_tests {
    use super::*;
    use chain::names::name;

    #[test]
    fn the_vault_s_own_key_is_approved_through_the_station_s_first_trading_driver() {
        assert_eq!(
            key_driver_of(&[name(b"hyperliquid")]),
            Some(name(b"hyperliquid"))
        );
        assert_eq!(key_driver_of(&[name(b"local")]), Some(name(b"local")));
        // Lighter registers a key of its own: the Safe's driver holds the
        // vault's key, whichever order the station lists them in.
        assert_eq!(
            key_driver_of(&[name(b"lighter"), name(b"robinhood")]),
            Some(name(b"robinhood"))
        );
        assert_eq!(key_driver_of(&[name(b"lighter")]), Some(name(b"lighter")));
        assert_eq!(key_driver_of(&[]), None);
    }
}
