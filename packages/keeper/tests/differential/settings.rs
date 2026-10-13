//! The RPC settings a candidate keeper is run with, and the vault timers
//! each profile is run against.
//!
//! The candidate's settings are the checked-in `config/chains.toml`'s
//! `[rpc.*]` tables for the chain under test, with a profile's changes on
//! top. The baseline (`main`) refuses an `[rpc]` table, so it is given none;
//! nor is a candidate that is `main` itself, which is then run at the
//! checked-in settings only.

use super::node::BlockTime;

/// Settings changed for the candidate, and the vault timers they are run
/// against.
#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    /// `[rpc.chains."evm:<id>"]` settings changed, by name.
    pub settings: Vec<(&'static str, u64)>,
    pub slot_lead: u64,
    pub slot_window: u64,
    pub halt_grace: u64,
    /// Why the timers or settings are what they are.
    pub why: String,
}

/// The demo timers: 10 s slots, a 5 s lead, a 3 s window.
pub const SLOT_INTERVAL: u64 = 10;
pub const SLOT_LEAD: u64 = 5;
pub const SLOT_WINDOW: u64 = 3;
/// A halt's grace, shortened from the contracts' 600 s floor so a run
/// sees it end, and still above `keeper_notice_ms` at its largest plus an
/// ask's timeout (the startup check of notice against a halt's grace).
pub const HALT_GRACE: u64 = 120;
/// How long the keeper's executor ask may take, as the startup check counts
/// it.
pub const ASK_TIMEOUT_MS: u64 = 5_000;

impl Profile {
    fn new(name: &str, settings: Vec<(&'static str, u64)>, why: &str) -> Profile {
        Profile {
            name: name.to_string(),
            settings,
            slot_lead: SLOT_LEAD,
            slot_window: SLOT_WINDOW,
            halt_grace: HALT_GRACE,
            why: why.to_string(),
        }
    }

    pub fn is_checked_in(&self) -> bool {
        self.settings.is_empty()
    }

    /// The checked-in settings, unchanged.
    pub fn checked_in() -> Profile {
        Profile::new(
            "checked-in settings",
            vec![],
            "the defaults and the chain's own overrides",
        )
    }

    /// The fastest pace and the shortest notice the settings accept.
    pub fn fastest() -> Profile {
        Profile::new(
            "fastest active round and notice",
            vec![("keeper_active_round_ms", 250), ("keeper_notice_ms", 250)],
            "keeper_active_round_ms and keeper_notice_ms at their 250 ms floor",
        )
    }

    /// The longest notice the settings accept.
    pub fn slowest_notice() -> Profile {
        Profile::new(
            "longest notice",
            vec![("keeper_notice_ms", 60_000)],
            "keeper_notice_ms at its 60 s ceiling; the halt's grace is 120 s, above 60 s and an ask",
        )
    }

    /// The shortest `head_lag_max_ms` at which the chain's heads still read
    /// as bounded: a block's time is whole seconds, so a head is up to a
    /// block and a second old when read.
    pub fn least_head_lag(block: BlockTime) -> Profile {
        let lag = block.millis() + 1_000 + 250;
        Profile::new(
            "least head lag that stays bounded",
            vec![("head_lag_max_ms", lag)],
            &format!(
                "head_lag_max_ms {lag}: a {} ms block, a second of whole-second block times and a margin",
                block.millis()
            ),
        )
    }

    /// The longest `head_lag_max_ms` the startup check accepts against the
    /// demo timers: its ceiling where the checked-in settings already miss
    /// the startup check that a bounded head sees a slot's requests (Base's),
    /// otherwise one under that check's line.
    pub fn most_head_lag(block: BlockTime) -> Profile {
        let lag = match block {
            BlockTime::TwoSeconds => 30_000,
            _ => SLOT_LEAD * 1_000 - block.millis() - 1,
        };
        Profile::new(
            "most head lag accepted",
            vec![("head_lag_max_ms", lag)],
            &format!("head_lag_max_ms {lag}"),
        )
    }

    /// The startup check that two observations fit a slot's window, met
    /// exactly: `2 * (active + read budget) = slotLead +
    /// slotWindow - block interval`.
    pub fn two_observations_exactly(block: BlockTime) -> Profile {
        let room = (SLOT_LEAD + SLOT_WINDOW) * 1_000 - block.millis();
        let active = 2_000;
        let budget = room / 2 - active;
        let mut profile = Profile::new(
            "two observations exactly fill the window",
            vec![
                ("keeper_active_round_ms", active),
                ("read_budget_ms", budget),
            ],
            &format!(
                "2 * ({active} + {budget}) = {room} ms = slotLead 5 s + slotWindow 3 s - a {} ms block",
                block.millis()
            ),
        );
        if block == BlockTime::TwoSeconds && budget == 1_000 {
            // The checked-in settings meet it exactly already; set them out
            // loud all the same.
            profile.why.push_str(" (the checked-in values)");
        }
        profile
    }

    /// The startup check that a bounded head sees a slot's requests, met
    /// with a millisecond to spare: `head_lag_max_ms + block
    /// interval < slotLead`.
    pub fn bounded_head_exactly(block: BlockTime) -> Profile {
        let lag = SLOT_LEAD * 1_000 - block.millis() - 1;
        Profile::new(
            "head lag a millisecond inside the slot lead",
            vec![("head_lag_max_ms", lag)],
            &format!(
                "head_lag_max_ms {lag} + a {} ms block = slotLead 5 s - 1 ms",
                block.millis()
            ),
        )
    }

    /// The startup check that notice and an ask end before a halt's grace,
    /// met by a second: the halt's grace one second above
    /// `keeper_notice_ms` and an ask's timeout, at the checked-in notice.
    pub fn grace_after_notice_exactly(checked_in_notice_ms: u64) -> Profile {
        let grace = (checked_in_notice_ms + ASK_TIMEOUT_MS) / 1_000 + 1;
        let mut profile = Profile::new(
            "halt grace a second past notice and an ask",
            vec![],
            &format!(
                "haltGrace {grace} s = keeper_notice_ms {checked_in_notice_ms} + ask {ASK_TIMEOUT_MS} ms + 1 s"
            ),
        );
        profile.halt_grace = grace;
        profile
    }
}

/// The checked-in `config/chains.toml`.
pub fn checked_in_config() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/chains.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {path:?}: {error}"))
}

/// The tables of a TOML text whose names start with `rpc`, in order, each
/// with its `key = value` lines: the settings are whole numbers, so nothing
/// more of TOML is needed.
fn rpc_tables(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut tables: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            current = name.starts_with("rpc").then(|| {
                tables.push((name.to_string(), Vec::new()));
                tables.len() - 1
            });
            continue;
        }
        if let (Some(at), Some((key, value))) = (current, line.split_once('=')) {
            tables[at]
                .1
                .push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    tables
}

/// A setting's checked-in value for a chain: its own override, or the
/// default.
pub fn checked_in_setting(config: &str, chain_id: u64, name: &str) -> Option<u64> {
    let tables = rpc_tables(config);
    let find = |table: &str| {
        tables
            .iter()
            .find(|(named, _)| named == table)
            .and_then(|(_, entries)| entries.iter().find(|(key, _)| key == name))
            .and_then(|(_, value)| value.parse().ok())
    };
    find(&format!("rpc.chains.\"evm:{chain_id}\"")).or_else(|| find("rpc.defaults"))
}

/// The candidate's `[rpc.*]` tables for a chain: the checked-in service and
/// default tables, and the chain's own with the profile's settings on top.
/// Empty when the checked-in file has none.
pub fn candidate_rpc_tables(config: &str, chain_id: u64, profile: &Profile) -> String {
    let tables = rpc_tables(config);
    if tables.is_empty() {
        return String::new();
    }
    let chain_table = format!("rpc.chains.\"evm:{chain_id}\"");
    let mut chain_entries: Vec<(String, String)> = tables
        .iter()
        .find(|(name, _)| *name == chain_table)
        .map(|(_, entries)| entries.clone())
        .unwrap_or_default();
    for (name, value) in &profile.settings {
        match chain_entries.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value.to_string(),
            None => chain_entries.push((name.to_string(), value.to_string())),
        }
    }
    let mut text = String::new();
    for (name, entries) in &tables {
        if !(name == "rpc.service" || name == "rpc.defaults") {
            continue;
        }
        text.push_str(&format!("[{name}]\n"));
        for (key, value) in entries {
            text.push_str(&format!("{key} = {value}\n"));
        }
        text.push('\n');
    }
    if !chain_entries.is_empty() {
        text.push_str(&format!("[{chain_table}]\n"));
        for (key, value) in &chain_entries {
            text.push_str(&format!("{key} = {value}\n"));
        }
    }
    text
}
