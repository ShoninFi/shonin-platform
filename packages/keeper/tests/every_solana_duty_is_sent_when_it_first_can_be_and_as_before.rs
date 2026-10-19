//! On a local Solana cluster with the keeper running as a process, each of
//! its duties is sent when the program first takes it, as the keeper always
//! has, while the keeper reads the cluster only at its wakes:
//!
//! - a deposit and a withdrawal are each valued in the first slot that
//!   prices them, inside its window, and settled after;
//! - a claim the cash covered (queued), unpaid a freeze period after its
//!   booking, freezes the vault at that moment;
//! - a killed vault is valued and closed, and its station stopped once the
//!   halt's grace is over;
//! - a retired vault is evicted once its wind-down is over.
//!
//! The deadlines are a day and more, so each is brought near by writing the
//! account's time back (Surfpool lets a test write any account): the cluster's
//! clock is never moved, and the keeper stays on its wakes throughout.
//!
//! With `SHONIN_BASELINE_KEEPER` naming another keeper binary (main's), the
//! same script also runs against it on a cluster of its own, and the two
//! keepers' sends are compared: the same calls for the same vaults, each
//! deadline-timed one sent no later after its moment than the other keeper
//! sent it, and each valuation in the same slot. A quiet minute at the end
//! measures each keeper's requests.

mod solana_world;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chain::Role;
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::anchor_lang::AccountSerialize;
use chain::solana::programs::vault::state::{self as vs, Claim, ClaimState, Request, Vault};
use chain::solana::rpc::Account;
use chain::solana::{Pubkey, Signer};
use solana_world::{DEPOSIT, Sent, World, baseline_keeper, this_keeper};

/// How far ahead of the script's start each deadline is brought.
const DUE_IN: u64 = 45;

/// What one run of the script found.
struct Run {
    sends: Vec<Sent>,
    /// The vaults, by their part in the script.
    vaults: BTreeMap<&'static str, Pubkey>,
    /// When each deadline fell, by the vault it is of.
    dues: BTreeMap<&'static str, u64>,
    /// The deposit's and the withdrawal's first moment to be priced.
    earliest: Vec<u64>,
    slot_interval: u64,
    slot_window: u64,
    /// Requests by method over the quiet minute at the end.
    quiet: BTreeMap<String, u64>,
    /// The looks (or rounds) the keeper made in it.
    quiet_looks: u64,
    states: BTreeMap<&'static str, String>,
}

impl Run {
    fn of(&self, what: &str, part: &str) -> Vec<&Sent> {
        let vault = self.vaults[part];
        self.sends
            .iter()
            .filter(|sent| sent.is(what, &vault))
            .collect()
    }

    /// When `what` was first sent for the vault playing `part`, after its
    /// deadline: how long after it.
    fn after_due(&self, what: &str, part: &str) -> Option<i64> {
        let due = self.dues[part] as i64;
        self.of(what, part)
            .iter()
            .map(|sent| sent.time - due)
            .find(|late| *late >= 0)
    }

    /// Every call, by what it is and the vault's part, with how many.
    fn calls(&self) -> BTreeMap<(String, &'static str), usize> {
        let mut calls = BTreeMap::new();
        for sent in self.sends.iter().filter(|sent| !sent.failed) {
            for (what, vault) in &sent.calls {
                let Some(part) = self
                    .vaults
                    .iter()
                    .find(|(_, at)| Some(**at) == *vault)
                    .map(|(part, _)| *part)
                else {
                    continue;
                };
                *calls.entry((what.to_string(), part)).or_default() += 1;
            }
        }
        calls
    }
}

/// How many looks (or rounds) a keeper's log, or its count of requests,
/// says it made: the candidate says each look at debug; a keeper that lists
/// a local cluster at every round reads the factory once a round.
fn looks_in(said: &str, requests: &BTreeMap<String, u64>) -> u64 {
    let said = said
        .lines()
        .filter(|line| {
            line.contains("looked at the cluster")
                || line.contains("read the cluster itself this round")
        })
        .count() as u64;
    if said > 0 {
        said
    } else {
        requests.get("getMultipleAccounts").copied().unwrap_or(0)
    }
}

fn counted(rows: Vec<(String, u64)>) -> BTreeMap<String, u64> {
    rows.into_iter().collect()
}

async fn run(binary: &Path, tag: &str) -> Option<Run> {
    let world = World::new(tag, "").await?;
    let client = &world.p.client;
    let platform = &world.p.platform;
    let (_, a) = world.vault().await;
    let (_, b) = world.vault().await;
    let (_, c) = world.vault().await;
    let (_, d) = world.vault().await;
    let keeper = world.keeper(binary, tag, "info,keeper::solana=debug");
    // Up, and its clock settled.
    tokio::time::sleep(Duration::from_secs(20)).await;
    let start = world.now().await;
    let due = start + DUE_IN;

    // A: a deposit.
    world.deposit(&a, DEPOSIT, 1).await;
    let viewer = world.role(Role::Viewer).pubkey();
    let deposit: Request = client
        .load(&vs::request_address(&a.vault, &viewer, 1).0)
        .await
        .unwrap()
        .unwrap();

    // B: a claim the cash covered, booked a freeze period before `due`.
    queued_claim(&world, &b, due).await;

    // C: killed, its grace over at `due`.
    let admin = world.role(Role::Admin);
    client
        .send(vec![platform.ix_kill(&admin.pubkey(), &c.vault)], &[admin])
        .await
        .unwrap();
    world
        .rewrite::<Vault>(&c.vault, |vault| {
            vault.halted_at = due - vault.timers.halt_grace;
        })
        .await;

    // D: retired, its wind-down over at `due`.
    let author = world.role(Role::Author);
    client
        .send(
            vec![platform.ix_retire(&author.pubkey(), &d.vault)],
            &[author],
        )
        .await
        .unwrap();
    world
        .rewrite::<Vault>(&d.vault, |vault| {
            vault.retired_at = due - vault.timers.wind_down;
        })
        .await;

    // A: once the deposit is priced, every share asked back.
    assert!(
        world
            .until(Duration::from_secs(90), || async {
                world.shares(&a).await != 0
            })
            .await,
        "{tag}: the deposit was never priced:\n{}",
        keeper.said()
    );
    world.redeem(&a, world.shares(&a).await, 2).await;
    let withdrawal: Request = client
        .load(&vs::request_address(&a.vault, &viewer, 2).0)
        .await
        .unwrap()
        .unwrap();

    // Until every deadline is well past and the withdrawal is paid.
    assert!(
        world
            .until(Duration::from_secs(120), || async {
                world.now().await > due + 30 && world.shares(&a).await == 0
            })
            .await,
        "{tag}: the withdrawal was never paid:\n{}",
        keeper.said()
    );

    // A quiet minute, measured.
    let requests = counted(keeper.requests().await);
    let looks = looks_in(&keeper.said(), &requests);
    tokio::time::sleep(Duration::from_secs(60)).await;
    let later = counted(keeper.requests().await);
    let quiet: BTreeMap<String, u64> = later
        .iter()
        .map(|(method, count)| {
            (
                method.clone(),
                count - requests.get(method).copied().unwrap_or(0),
            )
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    let quiet_looks = looks_in(&keeper.said(), &later) - looks;

    let vaults = BTreeMap::from([
        ("deposit", a.vault),
        ("queued claim", b.vault),
        ("killed", c.vault),
        ("retired", d.vault),
    ]);
    let mut states = BTreeMap::new();
    for (part, address) in &vaults {
        let vault: Vault = client.load(address).await.unwrap().unwrap();
        states.insert(*part, format!("{:?}", vault.state));
    }
    let interval_of: Vault = client.load(&a.vault).await.unwrap().unwrap();
    let sends = world
        .sends(&vaults.values().copied().collect::<Vec<_>>())
        .await;
    drop(keeper);
    Some(Run {
        sends,
        vaults,
        dues: BTreeMap::from([("queued claim", due), ("killed", due), ("retired", due)]),
        earliest: vec![deposit.earliest, withdrawal.earliest],
        slot_interval: interval_of.timers.slot_interval,
        slot_window: interval_of.timers.slot_window,
        quiet,
        quiet_looks,
        states,
    })
}

/// A claim of `v`'s that the cash covered (queued), booked a freeze period
/// before `due`, as the vault books one: its account at the vault's next
/// claim id, the vault's counts moved with it.
async fn queued_claim(world: &World, v: &VaultAccounts, due: u64) {
    let client = &world.p.client;
    let vault: Vault = client.load(&v.vault).await.unwrap().unwrap();
    let id = vault.claim_count + 1;
    let (address, bump) = vs::claim_address(&v.vault, id);
    let viewer = world.role(Role::Viewer).pubkey();
    let claim = Claim {
        id,
        owner: viewer,
        shares: 1_000_000,
        booked_at: due - vault.timers.freeze_after,
        last_attempt_at: 0,
        state: ClaimState::Queued,
        paid: 0,
        request: 0,
        payer: viewer,
        held: 0,
        bump,
    };
    let mut data = Vec::new();
    claim.try_serialize(&mut data).unwrap();
    client
        .rpc
        .set_account(
            &address,
            &Account {
                lamports: 10_000_000,
                owner: chain::solana::programs::vault::ID,
                executable: false,
                data,
            },
        )
        .await
        .unwrap();
    world
        .rewrite::<Vault>(&v.vault, |vault| {
            vault.claim_count = id;
            vault.queued_claims += 1;
        })
        .await;
}

/// The rules every run must keep, whichever keeper made it.
fn kept_the_rules(run: &Run, tag: &str) {
    let interval = run.slot_interval;
    // A: each request valued in the first slot that prices it, inside the
    // window, and settled after.
    let valued = run.of("value", "deposit");
    for earliest in &run.earliest {
        let slot = earliest.div_ceil(interval) * interval;
        let pricing = valued
            .iter()
            .find(|sent| sent.time as u64 >= *earliest)
            .unwrap_or_else(|| panic!("{tag}: nothing valued at or after {earliest}"));
        assert_eq!(
            pricing.time as u64 / interval * interval,
            slot,
            "{tag}: a request was priced in another slot than its first: {:#?}",
            run.sends
        );
        assert!(
            (pricing.time as u64) < slot + run.slot_window,
            "{tag}: a valuation landed outside its window"
        );
        assert!(
            run.of("settle", "deposit")
                .iter()
                .any(|sent| sent.slot > pricing.slot),
            "{tag}: not settled after its valuation"
        );
    }
    // B, C, D: each deadline met at its moment.
    for (what, part) in [
        ("freeze", "queued claim"),
        ("stop", "killed"),
        ("evict", "retired"),
    ] {
        let late = run
            .after_due(what, part)
            .unwrap_or_else(|| panic!("{tag}: no {what} for the {part} vault: {:#?}", run.sends));
        assert!(
            late <= 5,
            "{tag}: the {part} vault's {what} went {late} s after its moment"
        );
    }
    assert!(
        !run.of("close", "killed").is_empty(),
        "{tag}: the killed vault was never closed"
    );
    // Frozen, then closed like any halted vault whose station is empty.
    assert!(
        matches!(run.states["queued claim"].as_str(), "Frozen" | "Closed"),
        "{tag}: {:?}",
        run.states
    );
    assert_eq!(run.states["killed"], "Closed", "{tag}");
}

fn said(run: &Run, tag: &str) {
    eprintln!("== {tag}");
    for ((what, part), count) in run.calls() {
        eprintln!("  {part:>13}  {what:<32} x{count}");
    }
    for (what, part) in [
        ("freeze", "queued claim"),
        ("stop", "killed"),
        ("evict", "retired"),
    ] {
        eprintln!(
            "  {part} {what}: {:?} s after its moment",
            run.after_due(what, part)
        );
    }
    let total: u64 = run.quiet.values().sum();
    eprintln!(
        "  quiet minute: {total} requests in {} looks ({:.1} a look): {:?}",
        run.quiet_looks,
        total as f64 / run.quiet_looks.max(1) as f64,
        run.quiet
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_solana_duty_is_sent_when_it_first_can_be_and_as_the_keeper_before_sent_it() {
    // One cluster at a time: two Surfpools set up at once starve each other.
    let candidate = run(&this_keeper(), "candidate").await;
    let before = match (&candidate, baseline_keeper()) {
        (Some(_), Some(binary)) => run(&binary, "baseline").await,
        _ => None,
    };
    let Some(candidate) = candidate else {
        return;
    };
    said(&candidate, "this keeper");
    kept_the_rules(&candidate, "this keeper");
    // Read at its wakes: no more than a few requests a look when nothing is
    // sent.
    let total: u64 = candidate.quiet.values().sum();
    assert!(
        total <= 3 * candidate.quiet_looks.max(1),
        "{total} requests in {} looks of a quiet minute: {:?}",
        candidate.quiet_looks,
        candidate.quiet
    );
    let Some(before) = before else {
        eprintln!("no SHONIN_BASELINE_KEEPER: not compared with another keeper");
        return;
    };
    said(&before, "the baseline keeper");
    kept_the_rules(&before, "the baseline keeper");
    // The same calls for the same vaults.
    assert_eq!(
        candidate.calls().keys().collect::<Vec<_>>(),
        before.calls().keys().collect::<Vec<_>>(),
        "the keepers sent different calls"
    );
    // Each deadline met no later than the baseline met it (a second's
    // jitter allowed).
    for (what, part) in [
        ("freeze", "queued claim"),
        ("stop", "killed"),
        ("evict", "retired"),
    ] {
        let (ours, theirs) = (
            candidate.after_due(what, part).unwrap(),
            before.after_due(what, part).unwrap(),
        );
        assert!(
            ours <= theirs + 1,
            "the {part} vault's {what}: {ours} s after its moment, against {theirs}"
        );
    }
}
