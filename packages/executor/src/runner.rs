//! Running one strategy.
//!
//! Its component is instantiated once and its `run` is called once. Fuel is
//! metered so a runaway loop cannot take the machine, and an epoch deadline
//! makes it yield every few milliseconds so one strategy cannot starve
//! another. If it faults, it is started again from the venue's own state: the
//! cache it had is gone, and it was promised to nobody.

use std::path::Path;
use std::time::Duration;

use alloy::primitives::Address;
use anyhow::{Context, Result, anyhow};
use chain::{Client, KeyClient};
use tokio::sync::mpsc::UnboundedReceiver;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use crate::host::{Counters, Ctx, Strategy};
use crate::watch::VaultEvent;

/// How much fuel a strategy may burn in a second, until the mapping from fuel
/// to processor time is measured. It is a rate, not a budget: the strategy is
/// topped back up as it runs.
const FUEL_PER_SECOND: u64 = 2_000_000;

/// How long between the host taking control back, so no strategy can hold a
/// machine to itself.
const YIELD_INTERVAL: Duration = Duration::from_millis(10);

pub struct Runner {
    pub chain: Client,
    pub key: KeyClient,
    pub vault: Address,
    pub venue: Address,
    pub venue_id: String,
    pub config: Vec<u8>,
}

impl Runner {
    /// Run the strategy until it returns, or until it faults.
    pub async fn run(
        &self,
        engine: &Engine,
        component: &Path,
        account_events: UnboundedReceiver<Vec<u8>>,
        vault_events: UnboundedReceiver<VaultEvent>,
        restarts: u64,
    ) -> Result<Counters> {
        let bytes =
            std::fs::read(component).with_context(|| format!("reading {}", component.display()))?;
        let component = Component::new(engine, &bytes)
            .map_err(|error| anyhow!("loading the strategy: {error}"))?;

        let mut linker: Linker<Ctx> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|error| anyhow!("the WASI imports: {error}"))?;
        Strategy::add_to_linker::<_, HasSelf<Ctx>>(&mut linker, |ctx| ctx)
            .map_err(|error| anyhow!("the platform's imports: {error}"))?;

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
                venue: self.venue,
                venue_id: self.venue_id.clone(),
                cache: Default::default(),
                account_events: Some(account_events),
                vault_events: Some(vault_events),
                counters: Counters {
                    restarts,
                    ..Default::default()
                },
            },
        );

        store.set_fuel(FUEL_PER_SECOND)?;
        store.fuel_async_yield_interval(Some(FUEL_PER_SECOND / 100))?;
        store.set_epoch_deadline(1);
        store.epoch_deadline_async_yield_and_update(1);

        let instance = Strategy::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|error| anyhow!("instantiating the strategy: {error}"))?;

        let config = self.config.clone();
        let outcome = store
            .run_concurrent(async |accessor| instance.call_run(accessor, config).await)
            .await;

        let mut counters = store.data().counters;
        match outcome {
            Ok(Ok(Ok(()))) => {
                tracing::info!(vault = %self.vault, "the strategy finished");
            }
            Ok(Ok(Err(reason))) => {
                counters.faults += 1;
                tracing::warn!(vault = %self.vault, reason, "the strategy stopped itself");
            }
            Ok(Err(error)) | Err(error) => {
                counters.faults += 1;
                tracing::warn!(vault = %self.vault, %error, "the strategy faulted");
            }
        }
        Ok(counters)
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
