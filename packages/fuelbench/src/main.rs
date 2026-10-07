//! How much Wasmtime fuel a second of one core's time buys.
//!
//! The published prices charge processor time per core-hour, and an agent's
//! allowance is written in fuel a second, so the price list needs the rate
//! that turns one into the other. This measures it: it runs Wasm under the
//! engine and store settings the executor runs agents with (fuel on, epoch
//! interruption, an async yield every hundredth of the agent's fuel rate and
//! at every epoch tick) and divides the fuel each workload burned by the
//! processor time the thread running it spent.
//!
//! `fuelbench rate --guest <component>` times the workloads a guest exports
//! (the one in `guest/` is built for `wasm32-wasip2`); `fuelbench start
//! <component>...` reports what an agent's component spends being started.
//! It builds and runs the same on any machine, so the figure for the
//! machines agents run on comes from running it there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use serde::Serialize;
use wasmtime::component::{Component, Linker, ResourceTable, TypedFunc, Val};
use wasmtime::{Engine, ResourceLimiter, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// What the executor keeps an agent's fuel at: far more than anything burns,
/// so fuel never stops a workload and only decides when it yields.
const BANK: u64 = u64::MAX / 4;

/// How often the executor's epoch ticks, taking the machine back.
const YIELD_INTERVAL: Duration = Duration::from_millis(10);

/// The shortest call timed on its own: a call much shorter is mostly the
/// cost of calling.
const SHORTEST_CALL: Duration = Duration::from_millis(20);

#[derive(Parser)]
#[command(
    name = "fuelbench",
    about = "Measures Wasmtime fuel per second of processor time"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Time each workload a guest exports, and report the fuel it burns per
    /// second of one core's time.
    Rate {
        /// The guest component: one exporting `workloads` and `run`.
        #[arg(long)]
        guest: PathBuf,
        /// Processor time spent on each workload in each way it is run.
        #[arg(long, default_value_t = 10.0)]
        seconds: f64,
        /// How many trials that time is split into, for the spread.
        #[arg(long, default_value_t = 5)]
        trials: u32,
        /// The agent fuel rate whose yield interval the store is set with,
        /// as the executor sets it; the modest allowance by default.
        #[arg(long)]
        fuel_per_sec: Option<u64>,
        /// How many copies to run at once, each on its own thread: as many
        /// as the machine has processors, to time each one with its
        /// neighbours busy, as they are on a full machine.
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// The fuel each component spends being instantiated, and then running
    /// until it first calls the host. Every import is a stand-in that traps.
    Start {
        components: Vec<PathBuf>,
        #[arg(long)]
        fuel_per_sec: Option<u64>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let engine = engine()?;
    start_epoch_ticker(&engine);
    match args.command {
        Command::Rate {
            guest,
            seconds,
            trials,
            fuel_per_sec,
            threads,
            json,
        } => {
            let fuel_per_sec = fuel_per_sec.unwrap_or(shared::Allowances::modest().fuel_per_sec);
            let bytes =
                std::fs::read(&guest).with_context(|| format!("reading {}", guest.display()))?;
            let component = Component::new(&engine, &bytes)?;
            let plan = Plan {
                per_trial: Duration::from_secs_f64(seconds / trials.max(1) as f64),
                trials: trials.max(1),
                fuel_per_sec,
            };
            let report = rate(&engine, &component, &plan, threads.max(1))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_rate(&report);
            }
        }
        Command::Start {
            components,
            fuel_per_sec,
        } => {
            let fuel_per_sec = fuel_per_sec.unwrap_or(shared::Allowances::modest().fuel_per_sec);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            for path in components {
                runtime.block_on(start(&engine, &path, fuel_per_sec))?;
            }
        }
    }
    Ok(())
}

/// The engine as the executor builds it: the component model with async,
/// fuel metering and epoch interruption, and Wasmtime's defaults otherwise.
fn engine() -> Result<Engine> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model_async(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    Engine::new(&config).map_err(|error| anyhow!("starting the runtime: {error}"))
}

fn start_epoch_ticker(engine: &Engine) {
    let engine = engine.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(YIELD_INTERVAL);
            engine.increment_epoch();
        }
    });
}

/// A guest's WASI, and the most memory it has asked for.
struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    peak_memory: usize,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl ResourceLimiter for Ctx {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.peak_memory = self.peak_memory.max(desired);
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

/// A store set up as the executor sets up an agent's: fuel kept full, and a
/// yield at every epoch tick and every `yield_every` fuel (the executor's is
/// a hundredth of the agent's fuel rate; `None` is the tick alone).
fn store(engine: &Engine, yield_every: Option<u64>) -> Result<Store<Ctx>> {
    let mut store = Store::new(
        engine,
        Ctx {
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            peak_memory: 0,
        },
    );
    store.limiter(|ctx| ctx);
    store.set_fuel(BANK)?;
    store.fuel_async_yield_interval(yield_every)?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_async_yield_and_update(1);
    Ok(store)
}

/// The yield interval the executor sets for an agent's fuel rate.
fn yield_every(fuel_per_sec: u64) -> u64 {
    (fuel_per_sec / 100).max(1)
}

fn linker(engine: &Engine) -> Result<Linker<Ctx>> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    Ok(linker)
}

/// Processor time this thread has spent. The runtime has one thread, and the
/// guest runs on it, so this is the guest's time plus the host's around it.
fn thread_cpu() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

/// How long each workload is timed, and the agent it is timed as.
struct Plan {
    per_trial: Duration,
    trials: u32,
    fuel_per_sec: u64,
}

#[derive(Serialize)]
struct RateReport {
    machine: String,
    arch: &'static str,
    threads: usize,
    fuel_per_sec: u64,
    yield_every_fuel: u64,
    workloads: Vec<WorkloadRate>,
    /// The lowest of the workloads' medians, yielding only at the epoch:
    /// what the processor does with the guest's code.
    lowest_median: f64,
    highest_median: f64,
    /// The dearest yield, in microseconds of processor time.
    dearest_yield_us: f64,
    /// Fuel per core-second for an agent burning `fuel_per_sec`, from the
    /// lowest median and the dearest yield, taking the executor's hundred
    /// fuel yields a second and its hundred epoch ticks.
    sustained: f64,
}

#[derive(Serialize)]
struct WorkloadRate {
    name: String,
    fuel_per_iteration: f64,
    /// Fuel per second of processor time, yielding only at the epoch.
    epoch_only: Spread,
    /// The same, yielding as often as the executor has an agent at
    /// `fuel_per_sec` yield while it runs flat out.
    at_allowance: Spread,
    /// What one yield costs, in microseconds of processor time.
    yield_us: f64,
    peak_memory_mib: f64,
}

#[derive(Clone, Copy, Serialize)]
struct Spread {
    median: f64,
    min: f64,
    max: f64,
}

impl Spread {
    fn of(mut rates: Vec<f64>) -> Self {
        rates.sort_by(f64::total_cmp);
        Spread {
            median: rates[rates.len() / 2],
            min: rates[0],
            max: rates[rates.len() - 1],
        }
    }
}

/// One thread's timings of one workload.
struct Timed {
    fuel_per_iteration: f64,
    epoch_only: Vec<f64>,
    at_allowance: Vec<f64>,
    peak_memory: usize,
}

fn rate(engine: &Engine, component: &Component, plan: &Plan, threads: usize) -> Result<RateReport> {
    let per_thread = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(time_all(engine, component, plan))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow!("a timing thread panicked"))?
            })
            .collect::<Result<Vec<_>>>()
    })?;

    let interval = yield_every(plan.fuel_per_sec);
    let mut workloads = Vec::new();
    for (index, (name, _)) in per_thread[0].iter().enumerate() {
        let all = per_thread.iter().map(|timed| &timed[index].1);
        let epoch_only = Spread::of(all.clone().flat_map(|t| t.epoch_only.clone()).collect());
        let at_allowance = Spread::of(all.clone().flat_map(|t| t.at_allowance.clone()).collect());
        // The extra processor time a fuel of work took when it yielded
        // every `interval`, times the interval: the time one yield takes.
        let yield_secs =
            ((1.0 / at_allowance.median - 1.0 / epoch_only.median) * interval as f64).max(0.0);
        workloads.push(WorkloadRate {
            name: name.clone(),
            fuel_per_iteration: all.clone().map(|t| t.fuel_per_iteration).sum::<f64>()
                / threads as f64,
            epoch_only,
            at_allowance,
            yield_us: yield_secs * 1e6,
            peak_memory_mib: all.map(|t| t.peak_memory).max().unwrap_or(0) as f64
                / (1024.0 * 1024.0),
        });
    }

    let lowest_median = workloads
        .iter()
        .map(|w| w.epoch_only.median)
        .fold(f64::INFINITY, f64::min);
    let dearest_yield_us = workloads.iter().map(|w| w.yield_us).fold(0.0, f64::max);
    let rate = plan.fuel_per_sec as f64;
    let busy = rate / lowest_median + 200.0 * dearest_yield_us * 1e-6;
    Ok(RateReport {
        machine: machine(),
        arch: std::env::consts::ARCH,
        threads,
        fuel_per_sec: plan.fuel_per_sec,
        yield_every_fuel: interval,
        highest_median: workloads
            .iter()
            .map(|w| w.epoch_only.median)
            .fold(0.0, f64::max),
        lowest_median,
        dearest_yield_us,
        sustained: rate / busy,
        workloads,
    })
}

/// Every workload the guest exports, timed both ways on this thread.
async fn time_all(
    engine: &Engine,
    component: &Component,
    plan: &Plan,
) -> Result<Vec<(String, Timed)>> {
    let linker = linker(engine)?;
    let names = {
        let mut store = store(engine, None)?;
        let instance = linker.instantiate_async(&mut store, component).await?;
        let workloads = instance.get_typed_func::<(), (Vec<String>,)>(&mut store, "workloads")?;
        workloads.call_async(&mut store, ()).await?.0
    };
    let mut timed = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let index = index as u32;
        let (fuel_per_iteration, epoch_only, memory) =
            time_one(engine, &linker, component, index, None, plan).await?;
        let (_, at_allowance, _) = time_one(
            engine,
            &linker,
            component,
            index,
            Some(yield_every(plan.fuel_per_sec)),
            plan,
        )
        .await?;
        timed.push((
            name,
            Timed {
                fuel_per_iteration,
                epoch_only,
                at_allowance,
                peak_memory: memory,
            },
        ));
    }
    Ok(timed)
}

/// One workload in a fresh instance: the fuel an iteration takes, the fuel
/// per second of processor time in each trial, and the most memory it held.
async fn time_one(
    engine: &Engine,
    linker: &Linker<Ctx>,
    component: &Component,
    workload: u32,
    yield_every: Option<u64>,
    plan: &Plan,
) -> Result<(f64, Vec<f64>, usize)> {
    let mut store = store(engine, yield_every)?;
    let instance = linker.instantiate_async(&mut store, component).await?;
    let run = instance.get_typed_func::<(u32, u32), (u64,)>(&mut store, "run")?;

    // Enough iterations that one call is mostly work.
    let mut iterations = 1u32;
    loop {
        let (cpu, _) = timed(&mut store, run, workload, iterations).await?;
        if cpu >= SHORTEST_CALL || iterations >= u32::MAX / 2 {
            break;
        }
        iterations *= 2;
    }

    let mut rates = Vec::new();
    let (mut all_fuel, mut all_calls) = (0u128, 0u64);
    for _ in 0..plan.trials {
        let (mut cpu, mut fuel) = (Duration::ZERO, 0u64);
        while cpu < plan.per_trial {
            let (spent, burned) = timed(&mut store, run, workload, iterations).await?;
            cpu += spent;
            fuel += burned;
            all_calls += 1;
        }
        all_fuel += fuel as u128;
        rates.push(fuel as f64 / cpu.as_secs_f64());
    }
    let per_iteration = all_fuel as f64 / (all_calls as f64 * iterations as f64);
    Ok((per_iteration, rates, store.data().peak_memory))
}

/// One call of a workload: the processor time it took and the fuel it burned.
async fn timed(
    store: &mut Store<Ctx>,
    run: TypedFunc<(u32, u32), (u64,)>,
    workload: u32,
    iterations: u32,
) -> Result<(Duration, u64)> {
    let fuel = store.get_fuel()?;
    let cpu = thread_cpu();
    run.call_async(&mut *store, (workload, iterations)).await?;
    Ok((thread_cpu() - cpu, fuel - store.get_fuel()?))
}

fn print_rate(report: &RateReport) {
    println!(
        "{} ({}), {} thread(s) at once",
        report.machine, report.arch, report.threads
    );
    println!(
        "fuel per second of processor time: yielding at the epoch only, and as an agent \
         at {} fuel a second does when it runs flat out (every {} fuel)",
        report.fuel_per_sec, report.yield_every_fuel
    );
    println!(
        "{:<16} {:>10} {:>11} {:>11} {:>11} {:>12} {:>9} {:>8}",
        "workload", "fuel/iter", "epoch med", "min", "max", "yielding med", "yield us", "MiB"
    );
    for w in &report.workloads {
        println!(
            "{:<16} {:>10.0} {:>11.3e} {:>11.3e} {:>11.3e} {:>12.3e} {:>9.2} {:>8.1}",
            w.name,
            w.fuel_per_iteration,
            w.epoch_only.median,
            w.epoch_only.min,
            w.epoch_only.max,
            w.at_allowance.median,
            w.yield_us,
            w.peak_memory_mib
        );
    }
    println!(
        "medians {:.3e} to {:.3e} fuel per core-second; a yield costs at most {:.2} us",
        report.lowest_median, report.highest_median, report.dearest_yield_us
    );
    println!(
        "an agent burning {} fuel a second: {:.3e} fuel per core-second, with its yields",
        report.fuel_per_sec, report.sustained
    );
}

/// What a component spends being started, and then running until it first
/// needs the host.
async fn start(engine: &Engine, path: &Path, fuel_per_sec: u64) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let component = Component::new(engine, &bytes)?;
    // Every import a stand-in that traps, WASI's too: an agent's WASI may be
    // a newer minor version than this runtime's, and nothing it calls while
    // starting should need the host.
    let mut linker = Linker::new(engine);
    linker.define_unknown_imports_as_traps(&component)?;
    let mut store = store(engine, Some(yield_every(fuel_per_sec)))?;

    let cpu = thread_cpu();
    let instance = linker.instantiate_async(&mut store, &component).await?;
    let instantiating = BANK - store.get_fuel()?;
    let instantiating_cpu = thread_cpu() - cpu;
    let memory = store.data().peak_memory;

    let mut until_host = None;
    if let Some(run) = instance.get_func(&mut store, "run") {
        let mut results = vec![Val::Bool(false); run.ty(&store).results().len()];
        let before = store.get_fuel()?;
        let outcome = run.call_async(&mut store, &[], &mut results).await;
        let stopped = match outcome {
            Ok(()) => "returned".to_string(),
            Err(error) => format!("{error}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        };
        until_host = Some((before - store.get_fuel()?, stopped));
    }

    println!("{}", path.display());
    println!(
        "  instantiating: {instantiating} fuel, {:.3} ms, {:.1} MiB of memory",
        instantiating_cpu.as_secs_f64() * 1e3,
        memory as f64 / (1024.0 * 1024.0)
    );
    if let Some((fuel, stopped)) = until_host {
        println!(
            "  run, until it first needs the host: {fuel} fuel ({stopped}); {:.1} MiB of memory",
            store.data().peak_memory as f64 / (1024.0 * 1024.0)
        );
    }
    Ok(())
}

/// The processor's name, where the system says it.
fn machine() -> String {
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(line) = info.lines().find(|line| line.starts_with("model name"))
        && let Some((_, name)) = line.split_once(':')
    {
        return name.trim().to_string();
    }
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "an unnamed processor".to_string())
}
