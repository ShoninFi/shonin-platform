//! Running one strategy.
//!
//! Its component is instantiated once and its `run` is called once. Fuel is
//! metered and held to the rate the strategy declared, so a runaway loop
//! cannot take more of the machine than it pays for, and an epoch deadline
//! makes it yield every few milliseconds so one strategy cannot starve
//! another. If it faults, it is started again from the venue's own state: the
//! cache it had is gone, and it was promised to nobody.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use crate::feed::Reader;
use crate::host::{Agent, Allowance, Counters, Ctx, Place};
use crate::watch::VaultEvent;

/// The least fuel a second any strategy is held to, whatever its manifest
/// says.
const FUEL_FLOOR: u64 = 100_000;

/// The fuel a strategy's store starts with. It is never topped up: it is how
/// much the strategy has burnt that is read, and what it may burn is its
/// budget's to say ([`crate::pace`]). Held to the highest rate there is, a
/// strategy would take thousands of years to burn through it.
pub const BANK: u64 = u64::MAX / 4;

/// How long between the host taking control back, so no strategy can hold a
/// machine to itself.
const YIELD_INTERVAL: Duration = Duration::from_millis(10);

/// How much fuel a strategy burns between yields to the host: a hundredth of
/// a core-second, about [`YIELD_INTERVAL`] of work, the same for every
/// strategy whatever its rate. Fairness between strategies is the epoch's;
/// this only bounds how long one runs between yields however the clock
/// ticks. Tied to a strategy's own rate instead, a small rate would yield
/// every few microseconds, and a strategy running flat out would spend most
/// of its time yielding.
pub const YIELD_FUEL: u64 = shared::manifest::FUEL_PER_CORE_SECOND / 100;

/// The most host-side handles (WASI streams, pollables and the like) a
/// strategy may hold at once. A strategy drops each when it is done with it,
/// so a handful are live at a time; one that never drops them is refused the
/// next rather than growing the host's table without bound.
pub const HANDLES: usize = 4_096;

/// The most of a strategy's own words about why it stopped that go into the
/// log. The rest is the strategy's to keep: an error string is whatever it
/// says, and the log is the executor's.
pub const SAID_AT_MOST: usize = 1_024;

/// The longest a strategy is given to be instantiated: its start functions
/// (a module's constructors among them) are its own code, run and billed as
/// its `run` is. The time it spends parked for want of fuel is not counted,
/// so a heavy start at a small rate is given what it pays for. One that
/// takes longer is stopped as a fault, and started again later.
pub const INSTANTIATE_WITHIN: Duration = Duration::from_secs(30);

pub struct Runner {
    /// Where the vault is, and how its agent's requests reach it.
    pub place: Place,
    pub home_chain: String,
    /// The drivers this agent's manifest named.
    pub drivers: Vec<String>,
    /// Where the bundle's models are, if it carries any: each is loaded
    /// before the agent starts, and a model that won't load stops the start.
    pub models: Option<std::path::PathBuf>,
    /// Whether the manifest declares `network`: requests and WebSockets to
    /// any host.
    pub network: bool,
    /// What the manifest declared and the vault's tank is charged for. The
    /// host holds the strategy to it rather than to one figure for everyone.
    pub allowances: shared::Allowances,
    /// Raised once the vault's halting notice has been handed to the agent.
    pub halting_heard: Arc<AtomicBool>,
    /// Raised while the agent is running, which is what its capacity is
    /// billed for: from when its instantiation starts, which runs its own
    /// code, until the run ends, however it ends. Compiling it is not
    /// billed, and an agent that fails to compile never ran.
    pub running: Arc<AtomicBool>,
    /// The longest it is given to be instantiated ([`INSTANTIATE_WITHIN`]).
    pub instantiate_within: Duration,
    /// Set once the strategy is instantiated. How long it has run since says
    /// whether it ran for a good while: compiling and instantiating it are
    /// not running.
    pub instantiated: Arc<OnceLock<Instant>>,
    /// The stand-in venue's markets the agent follows, shared with the
    /// watcher that wakes it.
    pub following: crate::watch::Following,
}

/// How a strategy that was started stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stopped {
    /// Its `run` returned.
    Finished,
    /// It trapped, or its `run` returned an error.
    Faulted,
    /// It trapped while it was being instantiated, or took longer than it is
    /// given to be: its `run` was never reached.
    NotInstantiated,
}

impl Runner {
    /// Run the strategy until it returns, or until it faults, and say which.
    /// An error is a strategy that could not be started at all. Whatever is
    /// said of a strategy's failure may carry its own words (its error, its
    /// function names in a backtrace), and is cut to [`SAID_AT_MOST`].
    ///
    /// `component` is the bundle's component as it was read and checked
    /// against the hash the chain binds: these bytes, and nothing read again
    /// from the store, are what is run.
    pub async fn run(
        &self,
        engine: &Engine,
        component: &[u8],
        account_events: Reader<Vec<u8>>,
        vault_events: Reader<VaultEvent>,
        counters: Arc<Mutex<Counters>>,
    ) -> Result<Stopped> {
        // Compiled before anything is billed: this is the host's work, and
        // none of the strategy's code runs. Done once for each component, so
        // an agent started again isn't compiled again.
        let component = COMPILED.get(engine, component)?;

        let mut linker: Linker<Ctx> = Linker::new(engine);
        crate::host::link(&mut linker)?;

        let (fuel_per_sec, memory_mib) = enforced(&self.allowances);
        let (state, net) = services(
            self.network,
            &self.allowances,
            &self.home_chain,
            &self.place.vault(),
        );
        // The models are held in the agent's memory allowance: what they
        // take, its instance doesn't have.
        let memory = u64::from(memory_mib) * 1024 * 1024;
        let inference = match &self.models {
            Some(dir) => Some(Arc::new(crate::inference::Inference::load(
                dir,
                crate::inference::Budget::from_allowance(self.allowances.inference_ms_per_min),
                memory,
            )?)),
            None => None,
        };
        let models = inference.as_ref().map_or(0, |models| models.bytes());
        let memory = usize::try_from(memory.saturating_sub(models)).unwrap_or(usize::MAX);
        let mut store = Paced::new(
            Store::new(
                engine,
                Ctx {
                    // A strategy gets a clock, randomness and somewhere to print.
                    // No filesystem, no environment, no sockets of its own.
                    wasi: WasiCtxBuilder::new()
                        .max_random_size(crate::host::RANDOM_AT_MOST)
                        .inherit_stdout()
                        .inherit_stderr()
                        .allow_tcp(false)
                        .allow_udp(false)
                        .allow_ip_name_lookup(false)
                        .build(),
                    table: handle_table(),
                    place: self.place.clone(),
                    home_chain: self.home_chain.clone(),
                    drivers: self.drivers.clone(),
                    inference,
                    state,
                    http: Default::default(),
                    http_hooks: crate::net::Http { net: net.clone() },
                    net,
                    account_events: Some(account_events),
                    vault_events: Some(vault_events),
                    following: self.following.clone(),
                    counters: counters.clone(),
                    halting_heard: self.halting_heard.clone(),
                    memory: Allowance::new(memory),
                },
            ),
            crate::pace::budget_of(&self.home_chain, &self.place.vault(), fuel_per_sec),
            Box::new(|ctx: &mut Ctx| ctx.count(Counters::held_back)),
        )?;

        // The strategy may grow to the memory it declared and no further. Set
        // before instantiating, so the memories and tables the strategy
        // declares are counted as well as what it grows them to.
        store.limiter(|ctx| ctx);
        // And what it hands the host in one call (strings and lists, however
        // often they repeat the same bytes of its memory) is held to the same
        // allowance: past it, the call traps before the host copies more.
        store.set_hostcall_fuel(memory);

        let vault = self.place.vault();
        let fault = |why: String| {
            if let Ok(mut counters) = counters.lock() {
                counters.faults += 1;
            }
            let why = at_most(&why, SAID_AT_MOST).into_owned();
            tracing::warn!(vault, why, "the strategy stopped");
        };

        // Instantiating runs the strategy's own code (every module's start
        // function), so it is billed as running from here, held to the same
        // fuel, epoch and memory as `run`, and given only so long.
        let _running = Running::start(self.running.clone());
        let drive = store.drive();
        let instance = match drive
            .within(
                self.instantiate_within,
                Agent::instantiate_async(&mut *store, &component, &linker),
            )
            .await
        {
            Some(Ok(instance)) => {
                let _ = self.instantiated.set(Instant::now());
                instance
            }
            Some(Err(error)) => {
                fault(format!("instantiating the strategy: {error:#}"));
                return Ok(Stopped::NotInstantiated);
            }
            None => {
                fault(format!(
                    "instantiating the strategy took longer than {} s",
                    self.instantiate_within.as_secs_f64()
                ));
                return Ok(Stopped::NotInstantiated);
            }
        };

        let outcome = drive
            .run(store.run_concurrent(async |accessor| instance.call_run(accessor).await))
            .await;
        match outcome {
            Ok(Ok(Ok(()))) => {
                tracing::info!(vault, "the strategy finished");
                return Ok(Stopped::Finished);
            }
            Ok(Ok(Err(reason))) => fault(reason),
            // The whole chain, because the line that matters is usually the
            // last one rather than the backtrace above it.
            Ok(Err(error)) | Err(error) => fault(format!("{error:#}")),
        }
        Ok(Stopped::Faulted)
    }
}

/// What is done to a strategy's store each time it is parked for want of
/// fuel: counted, for its health report.
pub type OnParked<T> = Box<dyn FnMut(&mut T) + Send + Sync>;

/// A strategy's store, metered as it runs and held to its fuel budget.
///
/// It yields every [`YIELD_FUEL`] of work and every [`YIELD_INTERVAL`] of
/// time. At every tick of the engine's clock ([`crate::pace::CHECK_EVERY`])
/// what it used since the last is charged to the budget, and a strategy
/// whose budget is dry is parked there, between two instructions, until it
/// has filled. Whatever was used after the last tick is charged when the
/// store is dropped, however the run ended.
///
/// What it used is the fuel its own code burnt and the time the host spent
/// working for it ([`HostClock`]), a core's second being worth
/// [`shared::manifest::FUEL_PER_CORE_SECOND`]. The store is driven through
/// [`Drive`], so that only the time it is being run is counted.
pub struct Paced<T: 'static> {
    store: Store<T>,
    budget: Arc<crate::pace::Budget>,
    /// The fuel the store held when it was last charged.
    seen: Arc<std::sync::atomic::AtomicU64>,
    host: Arc<HostClock>,
    parked: Arc<Parked>,
}

impl<T: 'static> Paced<T> {
    pub fn new(
        mut store: Store<T>,
        budget: Arc<crate::pace::Budget>,
        mut parked: OnParked<T>,
    ) -> Result<Self> {
        store.set_fuel(BANK)?;
        store.fuel_async_yield_interval(Some(YIELD_FUEL))?;
        store.set_epoch_deadline(1);
        let seen = Arc::new(std::sync::atomic::AtomicU64::new(BANK));
        let host = Arc::new(HostClock::default());
        let parked_for = Arc::new(Parked::default());
        // Every way out of the strategy's code into the host's (an import,
        // a call the runtime makes for it, a return from an export) and
        // back passes through here: what lies between is the host's time.
        let watched = host.clone();
        store.call_hook(move |_, hook| {
            if hook.entering_host() {
                watched.enter();
            } else {
                watched.leave();
            }
            Ok(())
        });
        let mut yielded = tokio::time::Instant::now();
        let (charged, seen_by_checks, host_by_checks, parked_by_checks) = (
            budget.clone(),
            seen.clone(),
            host.clone(),
            parked_for.clone(),
        );
        store.epoch_deadline_callback(move |mut store| {
            use wasmtime::UpdateDeadline;
            // Fuel is never topped up, so the store holds less of it only
            // for what the strategy burnt.
            let left = store.get_fuel()?;
            let used = seen_by_checks
                .swap(left, Ordering::SeqCst)
                .saturating_sub(left)
                .saturating_add(host_by_checks.take());
            let now = tokio::time::Instant::now();
            let park = charged.spend(used, now);
            if park {
                parked(store.data_mut());
            }
            // The metering's own work is not the strategy's.
            host_by_checks.skip();
            if park {
                yielded = now;
                let (budget, parked) = (charged.clone(), parked_by_checks.clone());
                return Ok(UpdateDeadline::YieldCustom(
                    1,
                    Box::pin(async move {
                        let _parked = parked.start();
                        budget.refilled().await
                    }),
                ));
            }
            if now.duration_since(yielded) >= YIELD_INTERVAL {
                yielded = now;
                return Ok(UpdateDeadline::Yield(1));
            }
            Ok(UpdateDeadline::Continue(1))
        });
        Ok(Self {
            store,
            budget,
            seen,
            host,
            parked: parked_for,
        })
    }

    /// What drives the store: every future that runs the strategy is run
    /// through it.
    pub fn drive(&self) -> Drive {
        Drive {
            host: self.host.clone(),
            parked: self.parked.clone(),
        }
    }
}

/// The time the host spends working for a strategy, outside its own code:
/// in its imports, lifting and lowering what they take and give, in the
/// runtime's calls on its behalf, and in polling the work its calls set
/// going. Counted only while the strategy's store is being driven
/// ([`Drive`]): a call waiting on the network, or a strategy parked, takes
/// nothing of the machine. Charged at a core's rate, a nanosecond of it a
/// unit of fuel at the published [`shared::manifest::FUEL_PER_CORE_SECOND`].
///
/// The host's code is everything that runs between the strategy's code
/// handing over (calling an import, returning from an export) and taking
/// back (an import returning, an export called); the runtime switches
/// between the strategy's tasks only from the host's side, so this holds
/// however its tasks interleave.
#[derive(Debug)]
pub struct HostClock {
    state: Mutex<HostState>,
}

#[derive(Debug)]
struct HostState {
    /// Whether the host's code is the one running.
    in_host: bool,
    /// When the host's latest stretch of running began, while it runs.
    since: Option<Instant>,
    /// The host's time not yet charged, in nanoseconds.
    owed: u128,
}

impl Default for HostClock {
    fn default() -> Self {
        // Before the strategy's code first runs, the host is instantiating
        // it.
        Self {
            state: Mutex::new(HostState {
                in_host: true,
                since: None,
                owed: 0,
            }),
        }
    }
}

impl HostState {
    fn count(&mut self, now: Instant) {
        if let Some(since) = self.since.take() {
            self.owed = self
                .owed
                .saturating_add(now.saturating_duration_since(since).as_nanos());
        }
    }
}

impl HostClock {
    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The strategy's code handed over to the host's.
    fn enter(&self) {
        let mut state = self.state();
        state.in_host = true;
        state.since = Some(Instant::now());
    }

    /// The host's code handed back to the strategy's.
    fn leave(&self) {
        let mut state = self.state();
        state.count(Instant::now());
        state.in_host = false;
    }

    /// The store is being driven again.
    fn resume(&self) {
        let mut state = self.state();
        if state.in_host {
            state.since = Some(Instant::now());
        }
    }

    /// The store is no longer being driven: it waits, or is parked.
    fn pause(&self) {
        self.state().count(Instant::now());
    }

    /// Leave the time since the latest count out: work that is the host's
    /// own.
    fn skip(&self) {
        let mut state = self.state();
        if state.since.is_some() {
            state.since = Some(Instant::now());
        }
    }

    /// The host's time not yet charged, as fuel, counted up to now.
    pub fn take(&self) -> u64 {
        let mut state = self.state();
        let now = Instant::now();
        if state.since.is_some() {
            state.count(now);
            state.since = Some(now);
        }
        let owed = std::mem::take(&mut state.owed);
        let fuel =
            owed.saturating_mul(u128::from(shared::manifest::FUEL_PER_CORE_SECOND)) / 1_000_000_000;
        u64::try_from(fuel).unwrap_or(u64::MAX)
    }
}

/// How long a strategy has been parked for want of fuel.
#[derive(Debug, Default)]
struct Parked {
    state: Mutex<(Duration, Option<tokio::time::Instant>)>,
}

impl Parked {
    fn state(&self) -> std::sync::MutexGuard<'_, (Duration, Option<tokio::time::Instant>)> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Parked from now until what is returned is dropped.
    fn start(self: &Arc<Self>) -> ParkedNow {
        self.state().1 = Some(tokio::time::Instant::now());
        ParkedNow(self.clone())
    }

    /// All the time parked up to `now`.
    fn so_far(&self, now: tokio::time::Instant) -> Duration {
        let (total, since) = *self.state();
        total + since.map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }
}

struct ParkedNow(Arc<Parked>);

impl Drop for ParkedNow {
    fn drop(&mut self) {
        let mut state = self.0.state();
        if let Some(since) = state.1.take() {
            state.0 += tokio::time::Instant::now().saturating_duration_since(since);
        }
    }
}

/// Drives a strategy's store: runs a future that runs the strategy, so the
/// host's time is counted only while it is being polled.
#[derive(Clone)]
pub struct Drive {
    host: Arc<HostClock>,
    parked: Arc<Parked>,
}

impl Drive {
    /// Run `future`, counting the host's time only while it is polled.
    pub async fn run<F: std::future::Future>(&self, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| {
            self.host.resume();
            let polled = future.as_mut().poll(cx);
            self.host.pause();
            polled
        })
        .await
    }

    /// Run `future` for at most `limit`, not counting the time the strategy
    /// is parked for want of fuel: `None` if it took longer.
    pub async fn within<F: std::future::Future>(
        &self,
        limit: Duration,
        future: F,
    ) -> Option<F::Output> {
        let started = tokio::time::Instant::now();
        let mut future = std::pin::pin!(self.run(future));
        loop {
            let now = tokio::time::Instant::now();
            let spent = now
                .saturating_duration_since(started)
                .saturating_sub(self.parked.so_far(now));
            if spent >= limit {
                return None;
            }
            if let Ok(output) = tokio::time::timeout(limit - spent, future.as_mut()).await {
                return Some(output);
            }
        }
    }
}

impl<T: 'static> std::ops::Deref for Paced<T> {
    type Target = Store<T>;
    fn deref(&self) -> &Store<T> {
        &self.store
    }
}

impl<T: 'static> std::ops::DerefMut for Paced<T> {
    fn deref_mut(&mut self) -> &mut Store<T> {
        &mut self.store
    }
}

impl<T: 'static> Drop for Paced<T> {
    fn drop(&mut self) {
        let mut used = self.host.take();
        if let Ok(left) = self.store.get_fuel() {
            used = used.saturating_add(self.seen.swap(left, Ordering::SeqCst).saturating_sub(left));
        }
        self.budget.settle(used, tokio::time::Instant::now());
    }
}

/// What a start of an agent is given besides its imports: its state, empty
/// at every start, so an agent rebuilds from the venue; and its network,
/// whose usage is the vault's and carries on from one start to the next.
pub fn services(
    network: bool,
    allowances: &shared::Allowances,
    home_chain: &str,
    vault: &str,
) -> (crate::state::Store, crate::net::Net) {
    let net = crate::net::Net {
        declared: network,
        quotas: crate::net::Quotas::of(allowances),
        usage: crate::net::usage_of(home_chain, vault),
        reach: crate::net::Reach::Public,
    };
    (crate::state::Store::default(), net)
}

/// How many compiled components are kept: the few last started.
const COMPILED_KEPT: usize = 4;

/// The components compiled for this machine's agents, kept so an agent
/// started again (after a fault, say) is not compiled again: compiling is
/// the host's own work, billed to nobody.
static COMPILED: Compiled = Compiled::new(COMPILED_KEPT);

/// Compiled components by their hash, the latest used first, at most `kept`
/// of them.
struct Compiled {
    kept: usize,
    entries: Mutex<std::collections::VecDeque<(String, Component)>>,
    #[cfg(test)]
    compiles: std::sync::atomic::AtomicUsize,
}

impl Compiled {
    const fn new(kept: usize) -> Self {
        Self {
            kept,
            entries: Mutex::new(std::collections::VecDeque::new()),
            #[cfg(test)]
            compiles: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// `bytes` compiled for `engine`: as compiled before if it was, the
    /// least lately used dropped if that keeps one too many.
    fn get(&self, engine: &Engine, bytes: &[u8]) -> Result<Component> {
        let hash = shared::manifest::hash_bytes(bytes);
        if let Ok(mut entries) = self.entries.lock()
            && let Some(at) = entries.iter().position(|(kept, component)| {
                *kept == hash && Engine::same(component.engine(), engine)
            })
            && let Some(entry) = entries.remove(at)
        {
            let component = entry.1.clone();
            entries.push_front(entry);
            return Ok(component);
        }
        #[cfg(test)]
        self.compiles.fetch_add(1, Ordering::SeqCst);
        let component = Component::new(engine, bytes).map_err(|error| {
            anyhow!(
                "loading the strategy: {}",
                at_most(&format!("{error:#}"), SAID_AT_MOST)
            )
        })?;
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|(kept, _)| *kept != hash);
            entries.push_front((hash, component.clone()));
            entries.truncate(self.kept);
        }
        Ok(component)
    }
}

/// What of its allowances the host holds a strategy to as it runs: its fuel
/// a second, never below [`FUEL_FLOOR`], and its memory. The rest are billed
/// but held to nothing here, so a change to them alone is no reason to start
/// the strategy again.
pub fn enforced(allowances: &shared::Allowances) -> (u64, u32) {
    (
        allowances.fuel_per_sec.max(FUEL_FLOOR),
        allowances.memory_mib,
    )
}

/// The table a strategy's host-side handles are kept in, held to
/// [`HANDLES`].
fn handle_table() -> ResourceTable {
    let mut table = ResourceTable::new();
    table.set_max_capacity(HANDLES);
    table
}

/// `said`, cut to at most `most` bytes on a character boundary, with a note
/// of how much was left out.
pub fn at_most(said: &str, most: usize) -> std::borrow::Cow<'_, str> {
    if said.len() <= most {
        return said.into();
    }
    let mut end = most;
    while !said.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} more bytes)", &said[..end], said.len() - end).into()
}

/// Holds the flag that says the agent is running, and lowers it however the
/// run ends: returned from, or dropped mid-run.
struct Running(Arc<AtomicBool>);

impl Running {
    fn start(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(flag)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Keep the engine's clock ticking, so an epoch deadline means something:
/// at every tick a running strategy's fuel is charged, and every few it
/// yields.
pub fn start_epoch_ticker(engine: &Engine) {
    let engine = engine.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(crate::pace::CHECK_EVERY);
            engine.increment_epoch();
        }
    });
}

/// The engine every strategy runs in: the component model with async, fuel
/// metering and epoch interruption, which are what make a strategy something
/// the host can bound rather than something it hopes behaves.
pub fn engine() -> Result<Engine> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model_async(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    Engine::new(&config).map_err(|error| anyhow!("starting the runtime: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strategy yields after about the epoch's slice of a core's work,
    /// whatever its own rate: a strategy at a small rate is not made to
    /// yield every few microseconds, nor one at a large rate let run longer
    /// between yields than any other.
    #[test]
    fn a_strategy_yields_every_hundredth_of_a_core_second_whatever_its_rate() {
        let per_core = shared::manifest::FUEL_PER_CORE_SECOND;
        assert_eq!(YIELD_FUEL, per_core / 100);
        let slice = Duration::from_secs_f64(YIELD_FUEL as f64 / per_core as f64);
        assert_eq!(slice, YIELD_INTERVAL);
        // A yield costs microseconds: a million fuel or more between them
        // keeps that a small share of the work.
        const { assert!(YIELD_FUEL >= 1_000_000) };
    }

    /// A core module whose `spin` never returns, the busiest an agent can
    /// be: one loop, no calls, counting its turns in `$turns` so what it
    /// burnt can be told from the turns even where the store's own count of
    /// fuel lags. `turn(n)` makes `n` turns and returns, to weigh one.
    const SPINS: &str = r#"(module
        (global $turns (export "turns") (mut i64) (i64.const 0))
        (func (export "spin")
            (loop $on
                (global.set $turns (i64.add (global.get $turns) (i64.const 1)))
                (br $on)))
        (func (export "turn") (param $n i64)
            (loop $on
                (global.set $turns (i64.add (global.get $turns) (i64.const 1)))
                (local.set $n (i64.sub (local.get $n) (i64.const 1)))
                (br_if $on (i64.gt_s (local.get $n) (i64.const 0))))))"#;

    /// The fuel a store metered as an agent's, declared at `rate`, burns
    /// spinning for `over`, from the turns it made; and what its budget was
    /// charged.
    async fn fuel_spun(rate: u64, over: Duration) -> (u64, u64) {
        let engine = engine().unwrap();
        start_epoch_ticker(&engine);
        let module = wasmtime::Module::new(&engine, SPINS).unwrap();

        // What a turn of `spin` costs: a turn of `turn` less its own count
        // and test.
        let mut weighing = Store::new(&engine, ());
        weighing.set_fuel(BANK).unwrap();
        weighing.set_epoch_deadline(1 << 40);
        let instance = wasmtime::Instance::new(&mut weighing, &module, &[]).unwrap();
        let turn = instance
            .get_typed_func::<i64, ()>(&mut weighing, "turn")
            .unwrap();
        let mut cost = |turns: i64| {
            let before = weighing.get_fuel().unwrap();
            turn.call(&mut weighing, turns).unwrap();
            before - weighing.get_fuel().unwrap()
        };
        let per_turn = (cost(2_001) - cost(1_001)) / 1_000;
        // `local.get`, `i64.const`, `i64.sub`, `local.set`, `local.get`,
        // `i64.const`, `i64.gt_s` and `br_if` against `br`.
        let per_turn = per_turn - 7;

        let budget = Arc::new(crate::pace::Budget::new(rate, tokio::time::Instant::now()));
        let mut store =
            Paced::new(Store::new(&engine, ()), budget.clone(), Box::new(|_| {})).unwrap();
        let drive = store.drive();
        let instance = drive
            .run(wasmtime::Instance::new_async(&mut *store, &module, &[]))
            .await
            .unwrap();
        let spin = instance
            .get_typed_func::<(), ()>(&mut *store, "spin")
            .unwrap();
        let spun = tokio::time::timeout(over, drive.run(spin.call_async(&mut *store, ()))).await;
        assert!(spun.is_err(), "the spinning agent stopped by itself");
        let turns = instance
            .get_global(&mut *store, "turns")
            .unwrap()
            .get(&mut *store)
            .i64()
            .unwrap();
        drop(store);
        (turns as u64 * per_turn, budget.used())
    }

    /// An agent that declared ten million fuel a second and spins flat out
    /// uses no more than that over ten seconds, give or take the one slice
    /// of work ([`YIELD_FUEL`]) a loop without calls is seen in, and is
    /// given at least half of it; all it used is charged to its budget, but
    /// for the part of that slice it was stopped in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_spins_uses_no_more_fuel_a_second_than_it_declared() {
        let rate = 10_000_000;
        let over = Duration::from_secs(10);
        let (used, charged) = fuel_spun(rate, over).await;
        let most = rate * over.as_secs() + YIELD_FUEL;
        assert!(
            used <= most,
            "declared {rate} fuel a second, used {used} over ten seconds, past {most}"
        );
        assert!(
            used >= rate * over.as_secs() / 2,
            "declared {rate} fuel a second, was given only {used} over ten seconds"
        );
        assert!(
            charged <= used && used - charged <= YIELD_FUEL,
            "used {used} fuel, charged {charged}"
        );
    }

    #[test]
    fn the_agent_is_billed_as_running_only_while_the_flag_is_held() {
        let flag = Arc::new(AtomicBool::new(false));
        let held = Running::start(flag.clone());
        assert!(flag.load(Ordering::SeqCst));
        drop(held);
        assert!(!flag.load(Ordering::SeqCst));
    }

    /// A core module that calls its import `host.work` once from `go`.
    const CALLS_THE_HOST: &str = r#"(module
        (import "host" "work" (func $work))
        (func (export "go") (call $work)))"#;

    /// What the budget of a store declared at a large rate is charged for
    /// one call of `go`, whose import is `work`, driven as an agent's is.
    async fn charged_for(work: impl Fn(&mut wasmtime::Linker<()>) -> wasmtime::Result<()>) -> u64 {
        let engine = engine().unwrap();
        start_epoch_ticker(&engine);
        let module = wasmtime::Module::new(&engine, CALLS_THE_HOST).unwrap();
        let mut linker = wasmtime::Linker::new(&engine);
        work(&mut linker).unwrap();
        let budget = Arc::new(crate::pace::Budget::new(
            shared::manifest::FUEL_PER_CORE_SECOND,
            tokio::time::Instant::now(),
        ));
        let mut store =
            Paced::new(Store::new(&engine, ()), budget.clone(), Box::new(|_| {})).unwrap();
        let drive = store.drive();
        let instance = drive
            .run(linker.instantiate_async(&mut *store, &module))
            .await
            .unwrap();
        // What instantiating cost, left out.
        store.host.take();
        let before = budget.used();
        let go = instance
            .get_typed_func::<(), ()>(&mut *store, "go")
            .unwrap();
        drive.run(go.call_async(&mut *store, ())).await.unwrap();
        drop(store);
        budget.used() - before
    }

    /// The host working for an agent inside one of its imports is charged
    /// to its budget at a core's rate, though its own code burnt next to
    /// nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_host_s_work_inside_an_import_is_charged_at_a_core_s_rate() {
        let busy = Duration::from_millis(50);
        let charged = charged_for(|linker| {
            linker.func_wrap("host", "work", move || {
                let began = Instant::now();
                while began.elapsed() < busy {
                    std::hint::spin_loop();
                }
            })?;
            Ok(())
        })
        .await;
        let core = shared::manifest::FUEL_PER_CORE_SECOND as f64;
        let least = (busy.as_secs_f64() * core) as u64;
        assert!(
            charged >= least && charged < least * 2,
            "the host worked {busy:?} in an import, charged {charged} fuel"
        );
    }

    /// An import waiting (on the network, say) takes nothing of the
    /// machine, and is charged nothing for the wait.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_import_waiting_is_not_charged_for_the_wait() {
        let wait = Duration::from_millis(200);
        let charged = charged_for(|linker| {
            linker.func_wrap_async("host", "work", move |_, ()| {
                Box::new(async move { tokio::time::sleep(wait).await })
            })?;
            Ok(())
        })
        .await;
        // A twentieth of the wait at most: the host's own steps around it.
        let most =
            (wait.as_secs_f64() / 20.0 * shared::manifest::FUEL_PER_CORE_SECOND as f64) as u64;
        assert!(
            charged < most,
            "an import waited {wait:?}, charged {charged} fuel"
        );
    }

    /// Time an agent spends parked for want of fuel is not counted against
    /// the time it is given: work it pays for slowly is given time to
    /// finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn time_parked_is_not_counted_against_a_time_limit() {
        let engine = engine().unwrap();
        start_epoch_ticker(&engine);
        let module = wasmtime::Module::new(&engine, SPINS).unwrap();
        // Work of a few seconds of what the rate pays for, seen by the
        // budget a slice ([`YIELD_FUEL`]) at a time.
        let rate = 20_000_000;
        let budget = Arc::new(crate::pace::Budget::new(rate, tokio::time::Instant::now()));
        let mut store =
            Paced::new(Store::new(&engine, ()), budget.clone(), Box::new(|_| {})).unwrap();
        let drive = store.drive();
        let instance = drive
            .run(wasmtime::Instance::new_async(&mut *store, &module, &[]))
            .await
            .unwrap();
        let turn = instance
            .get_typed_func::<i64, ()>(&mut *store, "turn")
            .unwrap();
        let limit = Duration::from_millis(500);
        let began = Instant::now();
        let done = drive
            .within(limit, turn.call_async(&mut *store, 5_000_000))
            .await;
        assert!(done.is_some(), "slow work was cut short");
        assert!(began.elapsed() > 2 * limit, "{:?}", began.elapsed());
        drop(store);
        assert!(budget.used() >= rate, "{}", budget.used());
        // Work that never ends is still cut short.
        let rich = Arc::new(crate::pace::Budget::new(
            shared::manifest::FUEL_PER_CORE_SECOND,
            tokio::time::Instant::now(),
        ));
        let mut store = Paced::new(Store::new(&engine, ()), rich, Box::new(|_| {})).unwrap();
        let drive = store.drive();
        let instance = drive
            .run(wasmtime::Instance::new_async(&mut *store, &module, &[]))
            .await
            .unwrap();
        let spin = instance
            .get_typed_func::<(), ()>(&mut *store, "spin")
            .unwrap();
        assert!(
            drive
                .within(limit, spin.call_async(&mut *store, ()))
                .await
                .is_none()
        );
    }

    /// A component that does nothing, told apart from others by `tag`.
    fn a_component(tag: u32) -> Vec<u8> {
        format!("(component (core module (func (export \"f{tag}\"))))").into_bytes()
    }

    #[test]
    fn a_component_started_again_is_not_compiled_again_and_only_a_few_are_kept() {
        let engine = engine().unwrap();
        let compiled = Compiled::new(2);
        let compiles = || compiled.compiles.load(Ordering::SeqCst);
        compiled.get(&engine, &a_component(1)).unwrap();
        compiled.get(&engine, &a_component(1)).unwrap();
        assert_eq!(
            compiles(),
            1,
            "a component started again was compiled again"
        );

        // Two more: the first, least lately used, is no longer kept.
        compiled.get(&engine, &a_component(2)).unwrap();
        compiled.get(&engine, &a_component(3)).unwrap();
        assert_eq!(compiled.entries.lock().unwrap().len(), 2);
        assert_eq!(compiles(), 3);
        compiled.get(&engine, &a_component(1)).unwrap();
        assert_eq!(compiles(), 4);

        // Compiled for another engine, it is compiled for this one.
        compiled
            .get(&super::engine().unwrap(), &a_component(1))
            .unwrap();
        assert_eq!(compiles(), 5);
    }

    #[test]
    fn an_agent_started_again_has_empty_state_and_its_vault_s_bandwidth_so_far() {
        let allowances = shared::Allowances::modest();
        let (mut state, net) = services(true, &allowances, "base", "0xrestarted");
        state.set("state", "peak", b"100".to_vec()).unwrap();
        net.usage
            .bytes(&net.quotas, 1_000, std::time::Instant::now())
            .unwrap();
        assert!(net.declared);

        // Started again: nothing kept in state, the day's bytes still counted.
        let (state, net) = services(true, &allowances, "base", "0xrestarted");
        assert_eq!(state.get("state", "peak"), None);
        assert_eq!(net.usage.bytes_today(), 1_000);
        // Held to the vault's allowances.
        assert_eq!(net.quotas.per_minute, allowances.requests_per_min);
        assert_eq!(net.quotas.open, allowances.streams);
        assert_eq!(net.quotas.per_day, allowances.bytes_per_day);
        assert_eq!(
            net.reach,
            crate::net::Reach::Public,
            "an agent can reach inside the machine"
        );
    }

    #[test]
    fn a_strategy_holds_a_bounded_number_of_host_handles() {
        let mut table = handle_table();
        for _ in 0..HANDLES {
            table.push(()).expect("a handle within the bound");
        }
        assert!(table.push(()).is_err(), "a handle past the bound was given");
    }

    #[test]
    fn a_strategy_s_error_is_logged_at_most_a_kibibyte_of() {
        let short = "the venue said no";
        assert_eq!(at_most(short, SAID_AT_MOST), short);
        // Multi-byte characters across the cut are not split.
        let long = "é".repeat(SAID_AT_MOST);
        let said = at_most(&long, SAID_AT_MOST);
        let kept = said.split('…').next().unwrap();
        assert!(kept.len() <= SAID_AT_MOST, "{} bytes kept", kept.len());
        assert!(said.len() < long.len());
        assert!(said.ends_with(&format!("({} more bytes)", long.len() - kept.len())));
    }
}
