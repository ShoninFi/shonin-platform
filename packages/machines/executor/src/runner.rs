//! Running one strategy.
//!
//! Its component is instantiated once and its `run` is called once. Fuel is
//! metered so a runaway loop cannot take the machine, and an epoch deadline
//! makes it yield every few milliseconds so one strategy cannot starve
//! another. If it faults, it is started again from the venue's own state: the
//! cache it had is gone, and it was promised to nobody.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::Address;
use anyhow::{Context, Result, anyhow};
use chain::{Client, KeyClient};
use tokio::sync::mpsc::UnboundedReceiver;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use crate::host::{Agent, Counters, Ctx};
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

pub struct Runner {
    pub chain: Client,
    pub key: KeyClient,
    pub vault: Address,
    pub station: Address,
    pub venue: Address,
    pub usd: Address,
    pub factory: Address,
    pub home_chain: String,
    /// The driver this agent's manifest named.
    pub driver: String,
    /// What the manifest declared and the vault's tank is charged for. The
    /// host holds the strategy to it rather than to one figure for everyone.
    pub allowances: shared::Allowances,
}

impl Runner {
    /// Run the strategy until it returns, or until it faults.
    pub async fn run(
        &self,
        engine: &Engine,
        component: &Path,
        account_events: UnboundedReceiver<Vec<u8>>,
        vault_events: UnboundedReceiver<VaultEvent>,
        counters: Arc<Mutex<Counters>>,
    ) -> Result<()> {
        let bytes =
            std::fs::read(component).with_context(|| format!("reading {}", component.display()))?;
        let component = Component::new(engine, &bytes)
            .map_err(|error| anyhow!("loading the strategy: {error}"))?;

        let mut linker: Linker<Ctx> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|error| anyhow!("the WASI imports: {error}"))?;
        Agent::add_to_linker::<_, HasSelf<Ctx>>(&mut linker, |ctx| ctx)
            .map_err(|error| anyhow!("the platform's imports: {error}"))?;

        let fuel = self.allowances.fuel_per_sec.max(FUEL_FLOOR);
        let mut store = Store::new(
            engine,
            Ctx {
                // A strategy gets a clock, randomness and somewhere to print.
                // No filesystem, no environment, no sockets of its own.
                wasi: WasiCtxBuilder::new()
                    .inherit_stdout()
                    .inherit_stderr()
                    .build(),
                table: ResourceTable::new(),
                chain: self.chain.clone(),
                key: self.key.clone(),
                vault: self.vault,
                station: self.station,
                venue: self.venue,
                usd: self.usd,
                factory: self.factory,
                home_chain: self.home_chain.clone(),
                driver: self.driver.clone(),
                cache: Default::default(),
                account_events: Some(account_events),
                vault_events: Some(vault_events),
                counters: counters.clone(),
                memory_bytes: self.allowances.memory_mib as usize * 1024 * 1024,
            },
        );

        // The strategy may grow to the memory it declared and no further.
        store.limiter(|ctx| ctx);

        // Kept full; the declared rate decides how often the strategy yields
        // to the host rather than whether it may carry on.
        store.set_fuel(BANK)?;
        store.fuel_async_yield_interval(Some((fuel / 100).max(1)))?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

        let instance = Agent::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|error| anyhow!("instantiating the strategy: {error}"))?;

        let outcome = store
            .run_concurrent(async |accessor| instance.call_run(accessor).await)
            .await;

        let fault = |why: String| {
            if let Ok(mut counters) = counters.lock() {
                counters.faults += 1;
            }
            tracing::warn!(vault = %self.vault, why, "the strategy stopped");
        };
        match outcome {
            Ok(Ok(Ok(()))) => tracing::info!(vault = %self.vault, "the strategy finished"),
            Ok(Ok(Err(reason))) => fault(reason),
            // The whole chain, because the line that matters is usually the
            // last one rather than the backtrace above it.
            Ok(Err(error)) | Err(error) => fault(format!("{error:#}")),
        }
        Ok(())
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
