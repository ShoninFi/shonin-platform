//! Running one strategy.
//!
//! Its component is instantiated once and its `run` is called once. Fuel is
//! metered so a runaway loop cannot take the machine, and an epoch deadline
//! makes it yield every few milliseconds so one strategy cannot starve
//! another. If it faults, it is started again from the venue's own state: the
//! cache it had is gone, and it was promised to nobody.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use crate::feed::Reader;
use crate::host::{Agent, Allowance, Counters, Ctx, Place};
use crate::watch::VaultEvent;

/// The least fuel a second any strategy gets, whatever its manifest says. A
/// strategy with no fuel would never yield.
const FUEL_FLOOR: u64 = 100_000;

/// What a strategy's fuel is kept at. It is deliberately far more than any
/// strategy could burn between host calls: fairness between strategies is the
/// epoch's job, and the declared fuel rate is what the tank is charged for.
pub const BANK: u64 = u64::MAX / 4;

/// How long between the host taking control back, so no strategy can hold a
/// machine to itself.
const YIELD_INTERVAL: Duration = Duration::from_millis(10);

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
/// its `run` is. One that takes longer is stopped as a fault, and started
/// again later.
pub const INSTANTIATE_WITHIN: Duration = Duration::from_secs(30);

pub struct Runner {
    /// Where the vault is, and how its agent's requests reach it.
    pub place: Place,
    pub home_chain: String,
    /// The driver this agent's manifest named.
    pub driver: String,
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
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|error| anyhow!("the WASI imports: {error}"))?;
        Agent::add_to_linker::<_, HasSelf<Ctx>>(&mut linker, |ctx| ctx)
            .map_err(|error| anyhow!("the platform's imports: {error}"))?;

        let (fuel, memory_mib) = enforced(&self.allowances);
        let mut store = Store::new(
            engine,
            Ctx {
                // A strategy gets a clock, randomness and somewhere to print.
                // No filesystem, no environment, no sockets of its own.
                wasi: WasiCtxBuilder::new()
                    .inherit_stdout()
                    .inherit_stderr()
                    .build(),
                table: handle_table(),
                place: self.place.clone(),
                home_chain: self.home_chain.clone(),
                driver: self.driver.clone(),
                cache: Default::default(),
                account_events: Some(account_events),
                vault_events: Some(vault_events),
                counters: counters.clone(),
                halting_heard: self.halting_heard.clone(),
                memory: Allowance::new(memory_mib as usize * 1024 * 1024),
            },
        );

        // The strategy may grow to the memory it declared and no further. Set
        // before instantiating, so the memories and tables the strategy
        // declares are counted as well as what it grows them to.
        store.limiter(|ctx| ctx);

        // Kept full; the declared rate decides how often the strategy yields
        // to the host rather than whether it may carry on.
        store.set_fuel(BANK)?;
        store.fuel_async_yield_interval(Some((fuel / 100).max(1)))?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

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
        let instance = match tokio::time::timeout(
            self.instantiate_within,
            Agent::instantiate_async(&mut store, &component, &linker),
        )
        .await
        {
            Ok(Ok(instance)) => {
                let _ = self.instantiated.set(Instant::now());
                instance
            }
            Ok(Err(error)) => {
                fault(format!("instantiating the strategy: {error:#}"));
                return Ok(Stopped::NotInstantiated);
            }
            Err(_) => {
                fault(format!(
                    "instantiating the strategy took longer than {} s",
                    self.instantiate_within.as_secs_f64()
                ));
                return Ok(Stopped::NotInstantiated);
            }
        };

        let outcome = store
            .run_concurrent(async |accessor| instance.call_run(accessor).await)
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

/// Keep the engine's clock ticking, so an epoch deadline means something.
pub fn start_epoch_ticker(engine: &Engine) {
    let engine = engine.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(YIELD_INTERVAL);
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

    #[test]
    fn the_agent_is_billed_as_running_only_while_the_flag_is_held() {
        let flag = Arc::new(AtomicBool::new(false));
        let held = Running::start(flag.clone());
        assert!(flag.load(Ordering::SeqCst));
        drop(held);
        assert!(!flag.load(Ordering::SeqCst));
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
