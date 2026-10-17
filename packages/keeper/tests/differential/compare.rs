//! What each keeper sent, by duty, and how the candidate's sends compare
//! with the baseline's: the same transactions (contract, calldata and value)
//! in the same order within each duty, each in the same slot, and in blocks
//! no further from the baseline's than the baseline's own second run is.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolCall;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::closer::ICloser;
use chain::bindings::station::IStation;
use chain::bindings::vault::IVault;

use super::node::MinedTx;
use super::world::World;

alloy::sol! {
    interface IPendingWithdrawals {
        function withdrawPendingBalance(address owner, uint16 assetIndex, uint128 baseAmount) external;
    }
}

/// One keeper's send, as the tables show it.
#[derive(Clone, Debug)]
pub struct Sent {
    /// The duty of the design's EVM table it is part of.
    pub duty: &'static str,
    /// What is compared in order: the contract and the function, and for
    /// some the argument that tells one series from another (a claim, a
    /// key).
    pub series: String,
    /// The whole call, readably.
    pub label: String,
    pub to: Option<Address>,
    pub input: Bytes,
    pub value: U256,
    pub block: u64,
    pub timestamp: u64,
    pub slot: u64,
    pub ok: bool,
}

impl Sent {
    fn same_call(&self, other: &Sent) -> bool {
        self.to == other.to && self.input == other.input && self.value == other.value
    }
}

fn dollars(amount: U256) -> String {
    let dollars = amount.to::<u128>() as f64 / 1e6;
    format!("{dollars}")
}

/// Name a keeper's transaction by the duty it does.
pub fn describe(world: &World, tx: &MinedTx) -> Sent {
    let to_name = tx
        .to
        .map(|to| world.name_of(to))
        .unwrap_or_else(|| "creation".to_string());
    let t0 = world.timing.t0 as i64;
    let at = |time: u64| format!("t0{:+}", time as i64 - t0);
    let data = tx.input.as_ref();
    let selector: [u8; 4] = data
        .get(..4)
        .and_then(|four| four.try_into().ok())
        .unwrap_or_default();
    let (duty, series, detail): (&'static str, String, String) =
        if data.is_empty() && !tx.value.is_zero() {
            (
                "Gas stocking",
                format!("{to_name} transfer"),
                format!("{} wei", tx.value),
            )
        } else if selector == IVault::recordValuationCall::SELECTOR {
            (
                "Valuation",
                format!("{to_name} recordValuation"),
                String::new(),
            )
        } else if let Ok(call) = IVault::settleCall::abi_decode(data) {
            (
                "Settle",
                format!("{to_name} settle"),
                format!("max {}", call.max),
            )
        } else if let Ok(call) = IVault::closeValuationCall::abi_decode(data) {
            (
                "Close an open valuation",
                format!("{to_name} closeValuation"),
                at(call.time),
            )
        } else if let Ok(call) = IVault::settleClaimCall::abi_decode(data) {
            (
                "Claim try",
                format!("{to_name} settleClaim({})", call.claimId),
                String::new(),
            )
        } else if let Ok(call) = IVault::freezeCall::abi_decode(data) {
            (
                "Claim freeze",
                format!("{to_name} freeze({})", call.claimId),
                String::new(),
            )
        } else if let Ok(call) = IVault::serveProRataCall::abi_decode(data) {
            (
                "Serve units",
                format!("{to_name} serveProRata({})", world.name_of(call.holder)),
                String::new(),
            )
        } else if let Ok(call) = IVault::freezeUnitsCall::abi_decode(data) {
            (
                "Units freeze",
                format!("{to_name} freezeUnits({})", world.name_of(call.holder)),
                String::new(),
            )
        } else if let Ok(call) = IVault::keepCall::abi_decode(data) {
            if call.total.is_zero() {
                ("Daily fill", format!("{to_name} keep(fill)"), String::new())
            } else {
                (
                    "Bill collect",
                    format!("{to_name} keep(bill"),
                    format!("{} as of {})", dollars(call.total), at(call.asOf)),
                )
            }
        } else if selector == IVault::refreshRestrictedCall::SELECTOR {
            (
                "Daily restricted list",
                format!("{to_name} refreshRestricted"),
                String::new(),
            )
        } else if selector == IVault::cancelCall::SELECTOR {
            ("Cancel", format!("{to_name} cancel"), String::new())
        } else if selector == IVault::evictCall::SELECTOR {
            ("Evict", format!("{to_name} evict"), String::new())
        } else if selector == IVault::closeCall::SELECTOR {
            ("Close", format!("{to_name} close"), String::new())
        } else if let Ok(call) = IVault::settleClosedCall::abi_decode(data) {
            (
                "Closed vault",
                format!("{to_name} settleClosed({})", call.requestId),
                String::new(),
            )
        } else if let Ok(call) = IVault::claimClosedCall::abi_decode(data) {
            (
                "Closed vault",
                format!("{to_name} claimClosed({})", call.claimId),
                String::new(),
            )
        } else if let Ok(call) = IStation::revokeKeyCall::abi_decode(data) {
            let driver = String::from_utf8_lossy(call.driver.as_slice())
                .trim_end_matches('\0')
                .to_string();
            let key = world.name_of(call.key);
            let duty = if key == "skipped key" {
                "Skipped revokes"
            } else {
                "Revoke keys"
            };
            (
                duty,
                format!("{to_name} revokeKey({driver}, {key})"),
                String::new(),
            )
        } else if selector == IStation::stop_0Call::SELECTOR {
            ("Halt: stop", format!("{to_name} stop"), String::new())
        } else if selector == IStation::unwindCall::SELECTOR {
            ("Halt: unwind", format!("{to_name} unwind"), String::new())
        } else if selector == IStation::unwindStoppedCall::SELECTOR {
            (
                "Halt: unwind",
                format!("{to_name} unwindStopped"),
                String::new(),
            )
        } else if selector == IStation::settleCloseCall::SELECTOR {
            (
                "Delayed venue",
                format!("{to_name} settleClose"),
                String::new(),
            )
        } else if let Ok(call) = IStation::reclaimCall::abi_decode(data) {
            (
                "Delayed venue",
                format!("{to_name} reclaim"),
                world.name_of(call.venueVault),
            )
        } else if let Ok(call) = IPendingWithdrawals::withdrawPendingBalanceCall::abi_decode(data) {
            (
                "Delayed venue",
                format!("{to_name} withdrawPendingBalance"),
                format!(
                    "{} for {}",
                    dollars(U256::from(call.baseAmount)),
                    world.name_of(call.owner)
                ),
            )
        } else if let Ok(call) = ICloser::closeHaltedCall::abi_decode(data) {
            (
                "Halt: Closer",
                format!("{to_name} closeHalted({})", world.name_of(call.vault)),
                String::new(),
            )
        } else if let Ok(call) = IAttestationRegistry::blessCall::abi_decode(data) {
            (
                "Key relay",
                format!("{to_name} bless({})", world.name_of(call.key)),
                String::new(),
            )
        } else if let Ok(call) = IAttestationRegistry::setRootKeyCall::abi_decode(data) {
            (
                "Key relay",
                format!("{to_name} setRootKey({})", world.name_of(call.rootSigner)),
                String::new(),
            )
        } else {
            (
                "Unknown",
                format!("{to_name} 0x{}", alloy::hex::encode(selector)),
                String::new(),
            )
        };
    let label = if detail.is_empty() {
        series.clone()
    } else {
        format!("{series} {detail}")
    };
    let interval = world.timing.slot_interval.max(1);
    Sent {
        duty,
        series,
        label,
        to: tx.to,
        input: tx.input.clone(),
        value: tx.value,
        block: tx.block,
        timestamp: tx.timestamp,
        slot: tx.timestamp - tx.timestamp % interval,
        ok: tx.status,
    }
}

/// How many slots apart two sends of a series may be and still be taken as
/// the same send by two keepers, rather than one sent by one keeper only.
const GAP_SLOTS: u64 = 3;

/// Pair two keepers' sends of one series by time: the pairing that keeps
/// their order and costs least, a pair costing how far apart in time its
/// sends are (and as much again as a gap if their calls differ), and a send
/// left unpaired costing `gap`. A keeper that makes one send more (one a
/// reorganisation undid, made again) then leaves that one unpaired, not
/// every send after it paired with the wrong one.
fn align(x: &[Sent], y: &[Sent], gap: u64) -> Vec<(Option<usize>, Option<usize>)> {
    let (n, m) = (x.len(), y.len());
    let pair = |i: usize, j: usize| {
        let apart = x[i].timestamp.abs_diff(y[j].timestamp);
        if x[i].same_call(&y[j]) {
            apart
        } else {
            apart + gap
        }
    };
    // cost[i][j]: the least cost of pairing x[i..] with y[j..].
    let mut cost = vec![vec![0u64; m + 1]; n + 1];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            cost[i][j] = if i == n {
                (m - j) as u64 * gap
            } else if j == m {
                (n - i) as u64 * gap
            } else {
                (pair(i, j) + cost[i + 1][j + 1])
                    .min(gap + cost[i + 1][j])
                    .min(gap + cost[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pairs = Vec::new();
    while i < n || j < m {
        if i < n && j < m && cost[i][j] == pair(i, j) + cost[i + 1][j + 1] {
            pairs.push((Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if i < n && (j == m || cost[i][j] == gap + cost[i + 1][j]) {
            pairs.push((Some(i), None));
            i += 1;
        } else {
            pairs.push((None, Some(j)));
            j += 1;
        }
    }
    pairs
}

/// One line of the table: one send of a series, as each keeper made it.
#[derive(Clone, Debug)]
pub struct Row {
    pub duty: &'static str,
    pub series: String,
    pub baseline: Option<Sent>,
    pub again: Option<Sent>,
    pub candidate: Option<Sent>,
    /// Whether an outsider's change made it due, which the candidate may
    /// notice up to `keeper_notice_ms` later.
    pub noticed: bool,
    /// Why the candidate deliberately does not send what the baseline does,
    /// if it is one of those.
    pub expected: Option<&'static str>,
    /// What is wrong with the candidate's, if anything.
    pub fault: Option<String>,
}

/// Whether the baseline's send is one the candidate leaves out by design:
/// a `refreshRestricted` that would add nothing to the vault's list (the
/// candidate simulates it first; the baseline sends it every daily round).
fn left_out_by_design(
    world: &World,
    series: &str,
    nth: usize,
    sent: &Sent,
) -> Option<&'static str> {
    if sent.duty != "Daily restricted list" {
        return None;
    }
    let grows = sent
        .to
        .is_some_and(|vault| world.restricted_grows.contains(&vault))
        && nth == 0;
    (!grows && series.ends_with("refreshRestricted"))
        .then_some("adds nothing to the list; the candidate sends it only when it would")
}

/// The three runs side by side, and the verdict.
pub struct Comparison {
    pub rows: Vec<Row>,
    /// Per duty: the furthest the baseline's second run's blocks are from
    /// its first (measured this run, and the most recorded before), and the
    /// candidate's.
    pub duties: BTreeMap<&'static str, DutyBlocks>,
    /// What the scenario expects the baseline to send that it sent too few
    /// of (see `World::expected`): the run did not test it.
    pub missing: Vec<String>,
    pub passed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DutyBlocks {
    pub jitter_this_run: u64,
    pub jitter_recorded: u64,
    pub candidate: u64,
    pub rows: usize,
}

/// Compare the candidate's sends with the baseline's. `recorded_jitter` is
/// the most each duty's blocks have differed between two baseline runs
/// before, at this block time; `notice_ms` the candidate's notice bound.
pub fn compare(
    world: &World,
    baseline: &[MinedTx],
    again: &[MinedTx],
    candidate: &[MinedTx],
    recorded_jitter: &BTreeMap<String, u64>,
    notice_ms: u64,
) -> Comparison {
    let by_series = |sent: &[MinedTx]| {
        let mut series: BTreeMap<String, Vec<Sent>> = BTreeMap::new();
        for tx in sent {
            let described = describe(world, tx);
            series
                .entry(described.series.clone())
                .or_default()
                .push(described);
        }
        series
    };
    let (a, b, c) = (by_series(baseline), by_series(again), by_series(candidate));
    let mut names: Vec<&String> = a.keys().chain(b.keys()).chain(c.keys()).collect();
    names.sort();
    names.dedup();
    let block_ms = world.timing.block.millis();
    let notice_blocks = notice_ms.div_ceil(block_ms);
    let mut rows = Vec::new();
    for name in names {
        let empty = Vec::new();
        let (sa, sb, sc) = (
            a.get(name).unwrap_or(&empty),
            b.get(name).unwrap_or(&empty),
            c.get(name).unwrap_or(&empty),
        );
        // The baseline's sends of the series against the candidate's, and
        // against its own second run's, each paired by time (see [`align`]).
        let gap = GAP_SLOTS * world.timing.slot_interval;
        let with_again: Vec<(Option<usize>, Option<usize>)> = align(sa, sb, gap);
        let again_of = |ai: usize| {
            with_again
                .iter()
                .find(|(x, _)| *x == Some(ai))
                .and_then(|(_, y)| *y)
        };
        let mut triples: Vec<(Option<usize>, Option<usize>, Option<usize>)> = align(sa, sc, gap)
            .into_iter()
            .map(|(ai, ci)| (ai, ai.and_then(again_of), ci))
            .collect();
        for (ai, bi) in &with_again {
            if ai.is_none() {
                triples.push((None, *bi, None));
            }
        }
        for (ai, bi, ci) in triples {
            let (a_sent, b_sent, c_sent) =
                (ai.map(|i| &sa[i]), bi.map(|i| &sb[i]), ci.map(|i| &sc[i]));
            let any = a_sent.or(c_sent).or(b_sent).expect("one of them sent it");
            // Noticed: the first send of the series, by the keeper it is
            // judged by, at or after an outsider's change that made it due.
            let (reference, series) = match (a_sent, c_sent) {
                (Some(sent), _) => (sent, sa),
                (None, Some(sent)) => (sent, sc),
                _ => (any, sb),
            };
            let noticed = world
                .noticed
                .iter()
                .filter(|change| any.label.starts_with(&change.label))
                .any(|change| {
                    reference.timestamp >= change.at
                        && !series.iter().any(|other| {
                            other.timestamp >= change.at && other.timestamp < reference.timestamp
                        })
                });
            let expected = match (ai, c_sent) {
                (Some(nth), None) => left_out_by_design(world, name, nth, &sa[nth]),
                _ => None,
            };
            rows.push(Row {
                duty: any.duty,
                series: name.clone(),
                baseline: a_sent.cloned(),
                again: b_sent.cloned(),
                candidate: c_sent.cloned(),
                noticed,
                expected,
                fault: None,
            });
        }
    }
    rows.sort_by_key(|row| {
        let first = row
            .baseline
            .as_ref()
            .or(row.candidate.as_ref())
            .or(row.again.as_ref());
        first
            .map(|sent| (sent.block, sent.timestamp))
            .unwrap_or_default()
    });

    // The baseline against itself, per duty.
    let mut duties: BTreeMap<&'static str, DutyBlocks> = BTreeMap::new();
    for row in &rows {
        let entry = duties.entry(row.duty).or_default();
        entry.rows += 1;
        entry.jitter_recorded = recorded_jitter.get(row.duty).copied().unwrap_or(0);
        if let (Some(a), Some(b)) = (&row.baseline, &row.again) {
            entry.jitter_this_run = entry.jitter_this_run.max(a.block.abs_diff(b.block));
        }
        if !row.noticed
            && let (Some(a), Some(c)) = (&row.baseline, &row.candidate)
        {
            entry.candidate = entry.candidate.max(a.block.abs_diff(c.block));
        }
    }
    let notice_s = notice_ms.div_ceil(1_000);
    let mut passed = true;
    for row in &mut rows {
        let jitter = duties
            .get(row.duty)
            .map(|d| d.jitter_this_run.max(d.jitter_recorded))
            .unwrap_or(0);
        row.fault = if row.expected.is_some() {
            None
        } else {
            judge(row, jitter, notice_s, notice_blocks, world.timing.t0)
        };
        // A send the candidate could make only once a withheld log could be
        // checked may come up to its notice and a slot after that.
        if row.fault.is_some()
            && let (Some(a), Some(c)) = (&row.baseline, &row.candidate)
            && a.same_call(c)
            && let Some((_, until)) = world
                .withheld
                .iter()
                .find(|(label, _)| c.label.starts_with(label.as_str()))
            && c.timestamp <= until + notice_s + world.timing.slot_interval + 2
        {
            row.fault = None;
            row.expected = Some("waited for a withheld log to be checked");
        }
        passed &= row.fault.is_none();
    }
    // What the scenario is built to have sent must have been: otherwise
    // the keepers agree only in leaving it out.
    let mut missing = Vec::new();
    for expected in &world.expected {
        let sent = baseline
            .iter()
            .filter(|tx| describe(world, tx).label.starts_with(&expected.label))
            .count();
        if sent < expected.count {
            missing.push(format!(
                "{} ({}): the baseline sent {sent}, the scenario expects {}",
                expected.label, expected.what, expected.count
            ));
        }
    }
    passed &= missing.is_empty();
    Comparison {
        rows,
        duties,
        missing,
        passed,
    }
}

fn judge(row: &Row, jitter: u64, notice_s: u64, notice_blocks: u64, t0: u64) -> Option<String> {
    let rel = |time: u64| time as i64 - t0 as i64;
    let (a, b, c) = (&row.baseline, &row.again, &row.candidate);
    let same = |x: &Option<Sent>, y: &Option<Sent>| match (x, y) {
        (Some(x), Some(y)) => x.same_call(y) && x.ok == y.ok,
        (None, None) => true,
        _ => false,
    };
    if !same(c, a) && !same(c, b) {
        return Some(match (a, c) {
            (Some(_), None) => "the candidate did not send it".to_string(),
            (None, Some(_)) => "only the candidate sent it".to_string(),
            _ => "the candidate's call differs".to_string(),
        });
    }
    let (Some(a), Some(c)) = (a, c) else {
        return None;
    };
    let way = |c: u64, a: u64| if c < a { "earlier" } else { "later" };
    if row.noticed {
        if c.timestamp > a.timestamp + notice_s + 2 {
            return Some(format!(
                "noticed {} s after the baseline, past the {notice_s} s notice",
                c.timestamp - a.timestamp
            ));
        }
        // Noticed up to its notice later than the baseline; no earlier than
        // any other send may be.
        if c.block > a.block + jitter + notice_blocks {
            return Some(format!(
                "{} blocks later than the baseline, past {jitter} of jitter and {notice_blocks} of notice",
                c.block - a.block
            ));
        }
        if a.block > c.block + jitter {
            return Some(format!(
                "{} blocks earlier than the baseline; its own runs differ by {jitter} for this duty",
                a.block - c.block
            ));
        }
        return None;
    }
    // Where the baseline's two runs land in different slots, any slot from
    // one to the other is the baseline's own.
    let slot_ok = c.slot == a.slot
        || b.as_ref()
            .is_some_and(|b| (a.slot.min(b.slot)..=a.slot.max(b.slot)).contains(&c.slot));
    if !slot_ok {
        return Some(format!(
            "slot t0{:+}, {} than the baseline's t0{:+}",
            rel(c.slot),
            way(c.slot, a.slot),
            rel(a.slot)
        ));
    }
    let off = c.block.abs_diff(a.block);
    let within_again = b.as_ref().is_some_and(|b| {
        let (low, high) = (a.block.min(b.block), a.block.max(b.block));
        (low..=high).contains(&c.block)
    });
    if off > jitter && !within_again {
        return Some(format!(
            "{off} blocks {} than the baseline; its own runs differ by {jitter} for this duty",
            way(c.block, a.block)
        ));
    }
    None
}

impl Comparison {
    /// The table, every row, and the per-duty summary.
    pub fn table(&self, world: &World) -> String {
        let t0 = world.timing.t0 as i64;
        let rel = |time: u64| time as i64 - t0;
        let cell = |sent: &Option<Sent>| match sent {
            Some(sent) => format!(
                "{:>5} t0{:+4} t0{:+4}{}",
                sent.block,
                rel(sent.timestamp),
                rel(sent.slot),
                if sent.ok { "" } else { " REVERTED" }
            ),
            None => format!("{:>21}", "-"),
        };
        let mut text = String::new();
        for missing in &self.missing {
            let _ = writeln!(text, "NOT TESTED: {missing}");
        }
        if !self.missing.is_empty() {
            let _ = writeln!(text);
        }
        let _ = writeln!(
            text,
            "{:<26} {:<58} {:>21} {:>21} {:>21} {:>5} {:>5}  verdict",
            "duty",
            "tx",
            "baseline blk/ts/slot",
            "again blk/ts/slot",
            "candidate blk/ts/slot",
            "dC",
            "dB",
        );
        for row in &self.rows {
            let label = row
                .baseline
                .as_ref()
                .or(row.candidate.as_ref())
                .or(row.again.as_ref())
                .map(|sent| sent.label.clone())
                .unwrap_or_default();
            let delta = |x: &Option<Sent>| match (&row.baseline, x) {
                (Some(a), Some(x)) => format!("{:+}", x.block as i64 - a.block as i64),
                _ => "-".to_string(),
            };
            let verdict = match (&row.fault, row.noticed) {
                (Some(fault), _) => format!("FAIL: {fault}"),
                (None, _) if row.expected.is_some() => {
                    format!("expected: {}", row.expected.unwrap_or_default())
                }
                (None, true) => "ok (noticed)".to_string(),
                (None, false) => "ok".to_string(),
            };
            let _ = writeln!(
                text,
                "{:<26} {:<58} {} {} {} {:>5} {:>5}  {}",
                row.duty,
                truncate(&label, 58),
                cell(&row.baseline),
                cell(&row.again),
                cell(&row.candidate),
                delta(&row.candidate),
                delta(&row.again),
                verdict
            );
        }
        let _ = writeln!(text);
        let _ = writeln!(
            text,
            "{:<26} {:>5} {:>18} {:>18} {:>18}",
            "duty", "txs", "jitter this run", "jitter recorded", "candidate off by"
        );
        for (duty, blocks) in &self.duties {
            let _ = writeln!(
                text,
                "{:<26} {:>5} {:>18} {:>18} {:>18}",
                duty, blocks.rows, blocks.jitter_this_run, blocks.jitter_recorded, blocks.candidate
            );
        }
        text
    }
}

fn truncate(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        text.to_string()
    } else {
        let cut: String = text.chars().take(most - 1).collect();
        format!("{cut}…")
    }
}
