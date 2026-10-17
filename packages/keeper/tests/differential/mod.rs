//! Differential runs: `main`'s keeper (the baseline) and the keeper built
//! here (the candidate) against identical scripted chains, and what each
//! sent compared transaction by transaction.
//!
//! A run starts three anvils at once with the same chain id, block time and
//! genesis, sets the same world up on each (see [`world`]), and at the same
//! instant starts the baseline against the first and the candidate against
//! the third; the baseline again starts against the second half a round
//! later (`SHONIN_BASELINE_AGAIN_OFFSET_MS`, 1000 by default), since two
//! identical keepers started at the same instant act at the same moments
//! and would show none of the jitter a keeper's round phase gives. That second run measures
//! `main`'s own jitter in the same conditions: a candidate's
//! send may be as many blocks from the baseline's as the baseline's two runs
//! are from each other for that duty (or have been in any run recorded
//! before at that block time), and no more. Each keeper reaches its anvil
//! through a [`proxy::Proxy`] that counts its requests, and whose knobs can
//! make the node misbehave. A run whose baseline leaves out a send the
//! scenario is built to have made (`World::expected`) fails as not tested,
//! however alike the keepers' sends are.
//!
//! The baseline binary is named by `SHONIN_BASELINE_KEEPER`; the candidate is
//! this crate's keeper, or `SHONIN_CANDIDATE_KEEPER`'s (set it to the
//! baseline to run `main` against itself). Each run's tables, logs and
//! counts go to `SHONIN_DIFFERENTIAL_OUT` (the system's temporary directory
//! otherwise), and the baseline's jitter per duty is recorded there for later
//! runs.
//!
//! Each test file uses part of this, and the rest would be dead code to it.

#![allow(dead_code)]

pub mod compare;
pub mod executor;
pub mod keeper;
pub mod node;
pub mod proxy;
pub mod settings;
pub mod world;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;

use compare::Comparison;
use executor::Executor;
use keeper::{Keeper, Start};
use node::{Anvil, BlockTime, Rpc, sleep_until_unix, unix_now};
use proxy::Proxy;
use settings::Profile;
use world::{LEAD, LENGTH, Timing, World};

/// One run at a time in a test binary: three anvils and three keepers each
/// are as much as a run should share the machine with.
static RUNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The keepers' run, by who they are.
pub const WHO: [&str; 3] = ["baseline", "baseline again", "candidate"];

/// Something made to go wrong, on every chain alike unless it says
/// otherwise, for the fault cases.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    /// Every chain's clock this many seconds behind the wall: each head is
    /// that much older when read.
    HeadsBehind(u64),
    /// Every chain moves its clock this many seconds a block from just
    /// before the keepers start: no chain clock bound holds.
    FastClock(u64),
    /// At `t0 + at` on the wall, every chain goes back `depth` blocks: to a
    /// snapshot taken that many blocks before, whatever landed since undone.
    /// The blocks mined after it are stamped with the wall's time again.
    Reorg { at: i64, depth: u64 },
    /// Every chain stops mining at `t0 + at` on the wall for `pause`
    /// seconds; what is sent meanwhile lands when it starts again.
    MiningPause { at: i64, pause: u64 },
    /// The units vault's station's `value()` costs more gas than one read
    /// of a batch may.
    CostlyView,
    /// A third holder's units appear at `t0 + at`, and its `ProRataTaken`
    /// is emitted then on every chain, by one identical harness transaction
    /// (the only one any case sends once the keepers run). The candidate's
    /// node leaves `ProRataTaken` out of every log read, and fails every read
    /// of a header by number, from a second before until `t0 + until`.
    LogWithheld { at: i64, until: i64 },
}

impl Fault {
    /// How much longer than usual the run lasts on the wall for it: the
    /// blocks a reorganisation undoes are mined again.
    fn extra_seconds(&self, block: BlockTime) -> u64 {
        match self {
            Fault::Reorg { depth, .. } => (depth * block.millis()).div_ceil(1_000),
            _ => 0,
        }
    }
}

/// What a run found.
pub struct Report {
    pub comparison: Comparison,
    pub table: String,
    pub out: PathBuf,
}

/// The baseline binary, from `SHONIN_BASELINE_KEEPER`.
pub fn baseline_binary() -> PathBuf {
    let path = std::env::var("SHONIN_BASELINE_KEEPER")
        .expect("SHONIN_BASELINE_KEEPER names main's keeper binary");
    PathBuf::from(path)
}

/// The candidate binary: `SHONIN_CANDIDATE_KEEPER`'s, or this crate's.
pub fn candidate_binary() -> PathBuf {
    std::env::var("SHONIN_CANDIDATE_KEEPER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_keeper")))
}

/// Whether the candidate is the baseline itself: `main` against itself.
pub fn candidate_is_baseline() -> bool {
    let canonical = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
    canonical(baseline_binary()) == canonical(candidate_binary())
}

/// Run the scenario at `block` with the candidate on `profile`, print the
/// table, and fail the test if the candidate's sends differ from the
/// baseline's more than the baseline's differ from themselves.
pub async fn differs_no_more_than_the_baseline_from_itself(block: BlockTime, profile: Profile) {
    differs_no_more_than_the_baseline_with(block, profile, &[]).await;
}

/// [`differs_no_more_than_the_baseline_from_itself`], with `faults` made to
/// happen during the run.
pub async fn differs_no_more_than_the_baseline_with(
    block: BlockTime,
    profile: Profile,
    faults: &[Fault],
) {
    if candidate_is_baseline() && !profile.is_checked_in() {
        println!(
            "skipped: the candidate is the baseline, which takes no RPC settings ({})",
            profile.name
        );
        return;
    }
    let report = run(block, &profile, faults)
        .await
        .unwrap_or_else(|error| panic!("the run did not complete: {error:#}"));
    println!("{}", report.table);
    println!("tables, logs and counts: {}", report.out.display());
    assert!(
        report.comparison.missing.is_empty(),
        "the baseline did not send what the scenario is built to test, so the run tested none of it: {:?}",
        report.comparison.missing
    );
    assert!(
        report.comparison.passed,
        "the candidate's sends differ from the baseline's beyond its own jitter; see the table above"
    );
}

/// One run: set up, run the three keepers, compare.
pub async fn run(block: BlockTime, profile: &Profile, faults: &[Fault]) -> Result<Report> {
    let _one_at_a_time = RUNS.lock().await;
    let baseline = baseline_binary();
    let candidate = candidate_binary();
    let config = settings::checked_in_config();
    let chain_id = block.chain_id();
    let rpc_tables = if candidate_is_baseline() {
        String::new()
    } else {
        settings::candidate_rpc_tables(&config, chain_id, profile)
    };
    let notice_ms = settings::checked_in_setting(&config, chain_id, "keeper_notice_ms")
        .map(|checked_in| {
            profile
                .settings
                .iter()
                .find(|(name, _)| *name == "keeper_notice_ms")
                .map_or(checked_in, |(_, value)| *value)
        })
        // Without RPC settings the candidate keeps the baseline's two-second
        // round.
        .unwrap_or(2_000);

    let out = out_dir(block, profile, faults);
    std::fs::create_dir_all(&out)?;

    // Long enough for the setup to land before the keepers start.
    let setup_allowance = 12 * block.millis().div_ceil(1_000).max(1) + 10;
    let interval = settings::SLOT_INTERVAL;
    // The chains' clock, which may run behind the wall's.
    let behind = faults
        .iter()
        .find_map(|fault| match fault {
            Fault::HeadsBehind(seconds) => Some(*seconds),
            _ => None,
        })
        .unwrap_or(0);
    let chain_now = || unix_now() - behind;
    let t0 = (chain_now() + setup_allowance + LEAD).div_ceil(interval) * interval;
    let timing = Timing {
        t0,
        block,
        slot_interval: interval,
        slot_lead: profile.slot_lead,
        slot_window: profile.slot_window,
        halt_grace: profile.halt_grace,
        freeze_after: 3_600,
        activation_window: 3_600,
    };

    // Three anvils, started together.
    let genesis = (behind > 0).then(|| unix_now() - behind);
    let anvils = [
        Anvil::start(block, genesis)?,
        Anvil::start(block, genesis)?,
        Anvil::start(block, genesis)?,
    ];
    let rpcs: Vec<Rpc> = anvils.iter().map(Anvil::rpc).collect();
    for rpc in &rpcs {
        rpc.wait_up(Duration::from_secs(10)).await?;
    }
    let accounts = rpcs[0].accounts().await?;
    let mut world = World::plan(timing, chain_id, accounts[0], accounts[4]);
    if faults.contains(&Fault::CostlyView) {
        world.costly_view = true;
    }
    // A clock that runs fast from the keepers' start passes the staged
    // close's proving deadline before they first look: its first settle ends
    // it unpaid, and nothing is proved or delivered.
    if faults
        .iter()
        .any(|fault| matches!(fault, Fault::FastClock(_)))
    {
        world.expect_the_staged_close_to_lapse();
    }
    for fault in faults {
        if let Fault::LogWithheld { at, until } = fault {
            world.late_holder = Some(timing.at(*at));
            // The candidate may serve the holder only once it can check
            // the log: up to its notice after the withholding ends.
            let units = world.vault("V6").name;
            world
                .withheld
                .push((format!("{units} serveProRata(holder 3)"), timing.at(*until)));
        }
    }
    for rpc in &rpcs[1..] {
        if rpc.accounts().await? != accounts {
            bail!("the anvils' accounts differ");
        }
    }
    let (one, two, three) = tokio::join!(
        world.set_up(&rpcs[0]),
        world.set_up(&rpcs[1]),
        world.set_up(&rpcs[2])
    );
    for (who, done) in WHO.iter().zip([one, two, three]) {
        done.with_context(|| format!("setting up the {who}'s chain"))?;
    }
    let start_at = t0 - LEAD;
    if chain_now() >= start_at {
        bail!("the setup took past the keepers' start; lengthen the allowance");
    }

    let mut executors = Vec::new();
    let mut proxies = Vec::new();
    for anvil in &anvils {
        executors.push(Executor::serve(world.executor.clone(), anvil.rpc()).await);
        proxies.push(Proxy::start(&anvil.url).await);
    }
    write_plan(&out, &world, profile, &rpc_tables)?;

    for fault in faults {
        if let Fault::FastClock(seconds) = fault {
            for rpc in &rpcs {
                rpc.call("anvil_setBlockTimestampInterval", json!([seconds]))
                    .await?;
            }
        }
    }
    sleep_until_unix(start_at + behind).await;
    let first_blocks = heads(&rpcs).await?;
    let binaries = [&baseline, &baseline, &candidate];
    let mut keepers = Vec::new();
    // The baseline and the candidate together; the baseline again after.
    for i in [0, 2, 1] {
        if i == 1 {
            tokio::time::sleep(baseline_again_offset()).await;
        }
        let tables = if i == 2 { rpc_tables.as_str() } else { "" };
        keepers.push(Keeper::start(Start {
            who: WHO[i],
            binary: binaries[i],
            block,
            rpc: &proxies[i].url,
            executor: &executors[i].url,
            rpc_tables: tables,
            world: &world,
            dir: &out.join(WHO[i].replace(' ', "-")),
        })?);
    }
    keepers.sort_by_key(|keeper: &Keeper| WHO.iter().position(|who| *who == keeper.who));
    let started = Instant::now();

    // Watch for a keeper that stops (the candidate refusing its settings,
    // say) while the run goes on.
    let extra: u64 = faults.iter().map(|fault| fault.extra_seconds(block)).sum();
    let end_at = t0 + LENGTH + extra + behind;
    let mut actions = scheduled(faults, t0 + behind, block);
    let mut snapshots: Vec<serde_json::Value> = Vec::new();
    while unix_now() < end_at {
        let now = unix_now();
        while actions.first().is_some_and(|(at, _)| *at <= now) {
            let (_, action) = actions.remove(0);
            match action {
                Action::Snapshot => {
                    snapshots.clear();
                    for rpc in &rpcs {
                        snapshots.push(rpc.call("evm_snapshot", json!([])).await?);
                    }
                }
                Action::Revert => {
                    // Only while no keeper has a send waiting to be mined
                    // on any chain: one the return to an earlier block
                    // dropped would leave its keeper waiting a minute for a
                    // receipt, by chance of its phase alone.
                    until_no_send_waits(&rpcs).await?;
                    for (rpc, id) in rpcs.iter().zip(&snapshots) {
                        rpc.call("evm_revert", json!([id])).await?;
                    }
                    // A return to an earlier block sets anvil's clock back
                    // with it; a chain's clock keeps to the wall's.
                    keep_to_the_wall(&rpcs, behind).await?;
                }
                Action::StopMining => {
                    for rpc in &rpcs {
                        rpc.call("evm_setIntervalMining", json!([0])).await?;
                    }
                }
                Action::StartMining => {
                    // The first block after the pause is stamped with the
                    // wall's time, as a chain that stalled stamps it.
                    keep_to_the_wall(&rpcs, behind).await?;
                    for rpc in &rpcs {
                        rpc.call("evm_setIntervalMining", json!([block.millis() / 1_000]))
                            .await?;
                    }
                }
                Action::WithholdLogs => proxies[2].set(|knobs| {
                    knobs.dropped_logs = Some((None, Some(world::pro_rata_taken())));
                    knobs.failed_headers = true;
                }),
                Action::EmitLateHolder => {
                    let (one, two, three) = tokio::join!(
                        world.emit_late_holder(&rpcs[0]),
                        world.emit_late_holder(&rpcs[1]),
                        world.emit_late_holder(&rpcs[2])
                    );
                    one?;
                    two?;
                    three?;
                }
                Action::ReleaseLogs => proxies[2].set(|knobs| {
                    knobs.dropped_logs = None;
                    knobs.failed_headers = false;
                }),
            }
        }
        for keeper in &mut keepers {
            if let Some(status) = keeper.exited() {
                bail!(
                    "the {} exited early ({status}); its log ends:\n{}",
                    keeper.who,
                    tail(&keeper.logged(), 30)
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ran_for = started.elapsed();
    let mut counts = Vec::new();
    for (keeper, proxy) in keepers.iter().zip(&proxies) {
        counts.push(json!({
            "who": keeper.who,
            "requests": proxy.counts(),
            "debug_rpc": keeper.rpc_counts().await,
            "health": keeper.health().await,
        }));
    }
    drop(keepers);
    let last_blocks = heads(&rpcs).await?;

    let mut sent = Vec::new();
    for ((rpc, first), last) in rpcs.iter().zip(first_blocks).zip(last_blocks) {
        sent.push(rpc.sent_by(world.keeper, first, last).await?);
    }
    let alignment = alignment(&rpcs, &first_blocks, &last_blocks).await?;
    drop(anvils);

    // Jitter is recorded from, and judged by, runs without faults only.
    let recorded = recorded_jitter(block);
    let comparison = compare::compare(&world, &sent[0], &sent[1], &sent[2], &recorded, notice_ms);
    if faults.is_empty() {
        record_jitter(block, &comparison)?;
    }
    let mut table = format!(
        "{} at {} ms blocks, candidate on {} ({}){}; t0 = {t0}; ran {:.0} s; notice {} ms; baseline again started {} ms after\n{alignment}\n\n",
        if candidate_is_baseline() {
            "main against itself"
        } else {
            "candidate against main"
        },
        block.millis(),
        profile.name,
        profile.why,
        if faults.is_empty() {
            String::new()
        } else {
            format!(", with {faults:?}")
        },
        ran_for.as_secs_f64(),
        notice_ms,
        baseline_again_offset().as_millis(),
    );
    table.push_str(&comparison.table(&world));
    table.push_str(&format!(
        "\nverdict: {}\n",
        if !comparison.missing.is_empty() {
            "NOT TESTED: the baseline did not send what the scenario expects"
        } else if comparison.passed {
            "the candidate sends what the baseline sends"
        } else {
            "DIFFERENT"
        }
    ));
    std::fs::write(out.join("table.txt"), &table)?;
    std::fs::write(
        out.join("counts.json"),
        serde_json::to_string_pretty(&counts)?,
    )?;
    // Every keeper's sends as mined, for a table made again later.
    let sent_by_who: serde_json::Map<String, serde_json::Value> = WHO
        .iter()
        .zip(&sent)
        .map(|(who, txs)| Ok(((*who).to_string(), serde_json::to_value(txs)?)))
        .collect::<Result<_>>()?;
    std::fs::write(out.join("sent.json"), serde_json::to_string(&sent_by_who)?)?;
    Ok(Report {
        comparison,
        table,
        out,
    })
}

/// Wait until no chain has a transaction waiting to be mined, for at most a
/// few seconds.
async fn until_no_send_waits(rpcs: &[Rpc]) -> Result<()> {
    let started = Instant::now();
    loop {
        let mut waiting = 0;
        for rpc in rpcs {
            let status = rpc.call("txpool_status", json!([])).await?;
            waiting += node::quantity(&status["pending"])? + node::quantity(&status["queued"])?;
        }
        if waiting == 0 || started.elapsed() > Duration::from_secs(5) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Set every chain's clock to the wall's (less `behind`), on each alike.
async fn keep_to_the_wall(rpcs: &[Rpc], behind: u64) -> Result<()> {
    let now = unix_now() - behind;
    for rpc in rpcs {
        rpc.call("anvil_setTime", json!([now])).await?;
    }
    Ok(())
}

/// What a fault does at a moment of the run.
#[derive(Debug)]
enum Action {
    Snapshot,
    Revert,
    StopMining,
    StartMining,
    WithholdLogs,
    EmitLateHolder,
    ReleaseLogs,
}

/// The faults' actions, by the wall-clock second they are due, in order.
fn scheduled(faults: &[Fault], t0_on_the_wall: u64, block: BlockTime) -> Vec<(u64, Action)> {
    let at = |offset: i64| (t0_on_the_wall as i64 + offset) as u64;
    let mut actions = Vec::new();
    for fault in faults {
        match fault {
            Fault::Reorg { at: when, depth } => {
                let back = (depth * block.millis()).div_ceil(1_000) as i64;
                actions.push((at(*when - back), Action::Snapshot));
                actions.push((at(*when), Action::Revert));
            }
            Fault::MiningPause { at: when, pause } => {
                actions.push((at(*when), Action::StopMining));
                actions.push((at(*when + *pause as i64), Action::StartMining));
            }
            Fault::LogWithheld { at: when, until } => {
                actions.push((at(*when - 1), Action::WithholdLogs));
                actions.push((at(*when), Action::EmitLateHolder));
                actions.push((at(*until), Action::ReleaseLogs));
            }
            _ => {}
        }
    }
    actions.sort_by_key(|(at, _)| *at);
    actions
}

/// How long after the baseline its second run starts.
pub fn baseline_again_offset() -> Duration {
    Duration::from_millis(
        std::env::var("SHONIN_BASELINE_AGAIN_OFFSET_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(1_000),
    )
}

/// The three chains' heads, read together.
async fn heads(rpcs: &[Rpc]) -> Result<[u64; 3]> {
    let (one, two, three) = tokio::join!(rpcs[0].head(), rpcs[1].head(), rpcs[2].head());
    Ok([one?, two?, three?])
}

/// How alike the three chains' clocks are: blocks of the same number whose
/// times differ.
async fn alignment(rpcs: &[Rpc], first: &[u64; 3], last: &[u64; 3]) -> Result<String> {
    let from = first.iter().copied().max().unwrap_or(0);
    let to = last.iter().copied().min().unwrap_or(0);
    let mut differ = 0;
    let mut most = 0u64;
    let mut checked = 0;
    let step = ((to - from) / 40).max(1);
    let mut number = from;
    while number <= to {
        let mut times = Vec::new();
        for rpc in rpcs {
            times.push(rpc.block_time(Some(number)).await?.1);
        }
        let spread = times.iter().max().unwrap_or(&0) - times.iter().min().unwrap_or(&0);
        if spread != 0 {
            differ += 1;
        }
        most = most.max(spread);
        checked += 1;
        number += step;
    }
    Ok(format!(
        "chains' clocks: of {checked} blocks sampled ({from}..={to}), {differ} differ in time across the three, by at most {most} s"
    ))
}

fn out_root() -> PathBuf {
    std::env::var("SHONIN_DIFFERENTIAL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("keeper-differential"))
}

fn out_dir(block: BlockTime, profile: &Profile, faults: &[Fault]) -> PathBuf {
    let name: String = format!(
        "{}{}",
        profile.name,
        if faults.is_empty() {
            String::new()
        } else {
            format!(" {faults:?}")
        }
    )
    .chars()
    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
    .collect();
    out_root().join(format!(
        "{}ms-{name}-{}-{}",
        block.millis(),
        if candidate_is_baseline() {
            "main-vs-main"
        } else {
            "candidate"
        },
        unix_now()
    ))
}

fn jitter_file() -> PathBuf {
    out_root().join("baseline-jitter.jsonl")
}

/// The most each duty's blocks have differed between the baseline's two
/// runs, in every run recorded at this block time.
fn recorded_jitter(block: BlockTime) -> BTreeMap<String, u64> {
    let mut most = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(jitter_file()) else {
        return most;
    };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry["block_ms"] != json!(block.millis()) {
            continue;
        }
        if let Some(duties) = entry["jitter"].as_object() {
            for (duty, blocks) in duties {
                let blocks = blocks.as_u64().unwrap_or(0);
                let at = most.entry(duty.clone()).or_insert(0);
                *at = (*at).max(blocks);
            }
        }
    }
    most
}

fn record_jitter(block: BlockTime, comparison: &Comparison) -> Result<()> {
    let jitter: BTreeMap<&str, u64> = comparison
        .duties
        .iter()
        .map(|(duty, blocks)| (*duty, blocks.jitter_this_run))
        .collect();
    std::fs::create_dir_all(out_root())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(jitter_file())?;
    writeln!(
        file,
        "{}",
        json!({"block_ms": block.millis(), "at": unix_now(), "jitter": jitter})
    )?;
    Ok(())
}

fn write_plan(out: &Path, world: &World, profile: &Profile, rpc_tables: &str) -> Result<()> {
    let mut text = format!(
        "t0 = {}; keepers start at t0-{LEAD}; the run ends at t0+{LENGTH}\nprofile: {} ({})\n\n",
        world.timing.t0, profile.name, profile.why
    );
    for at in &world.vaults {
        text.push_str(&format!(
            "{:<18} vault {} station {}: {}\n",
            at.name, at.vault, at.station, at.drives
        ));
    }
    text.push_str("\ncandidate's RPC settings:\n");
    text.push_str(if rpc_tables.is_empty() {
        "(none)\n"
    } else {
        rpc_tables
    });
    std::fs::write(out.join("plan.txt"), text)?;
    Ok(())
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
