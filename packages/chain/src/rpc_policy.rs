//! Bounded RPC settings: how often each service reads a chain, how much one
//! read may cover, and the checks a vault's timers put on them at startup.
//!
//! Every value lives in `config/chains.toml`: `[rpc.defaults]` for every
//! chain, `[rpc.chains."kind:id"]` for the values one chain changes, and
//! `[rpc.service]` for the process-wide polls. The checked-in file is also
//! where the defaults come from when no file is given, so there is one set of
//! numbers. Each value has a range, and a value outside it is refused with the
//! scope, the field, the value and the range (or the field it conflicts with)
//! named.
//!
//! The ranges alone cannot say whether a vault's timers can be met: that
//! takes the vault's timers and the chain's block interval, which only the
//! keeper knows. [`RpcPolicy::two_observations_fit`],
//! [`RpcPolicy::bounded_head_sees_requests`] and
//! [`RpcPolicy::notice_before_deadlines`] are those checks, and [`judge`]
//! decides what a failed one means.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::config::{ChainKey, ConfigError};

macro_rules! settings {
    ($($name:ident),+ $(,)?) => {
        /// One chain's RPC settings. Durations are whole milliseconds unless
        /// the name says otherwise.
        #[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct RpcPolicy { $(pub $name: u64,)+ }

        /// Only the values one chain overrides.
        #[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct RpcOverrides { $($name: Option<u64>,)+ }

        impl RpcOverrides {
            /// `policy` with this chain's values in place of its own.
            pub fn apply(&self, mut policy: RpcPolicy) -> RpcPolicy {
                $(if let Some(value) = self.$name { policy.$name = value; })+
                policy
            }
        }

        impl RpcPolicy {
            /// Every setting's name, as `chains.toml` spells it.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($name),)+];

            /// A setting's value by its name.
            pub fn get(&self, field: &str) -> Option<u64> {
                match field {
                    $(stringify!($name) => Some(self.$name),)+
                    _ => None,
                }
            }

            #[cfg(test)]
            fn set(&mut self, field: &str, value: u64) {
                match field {
                    $(stringify!($name) => self.$name = value,)+
                    _ => panic!("no RPC setting is called {field}"),
                }
            }
        }
    };
}

settings!(
    // Keeper (EVM)
    keeper_active_round_ms,
    keeper_notice_ms,
    keeper_log_scan_ms,
    receipt_poll_ms,
    keeper_simulation_initial_ms,
    keeper_simulation_max_ms,
    keeper_simulation_backoff_factor,
    keeper_rate_limit_initial_ms,
    keeper_rate_limit_max_ms,
    keeper_rate_limit_backoff_factor,
    key_poll_ms,
    key_look_ms,
    keeper_balance_floor_micro,
    // Chain clock and observation
    head_lag_max_ms,
    head_ahead_max_ms,
    clock_settle_observations,
    read_budget_ms,
    view_batch_reads,
    view_outer_gas,
    view_inner_gas,
    view_batch_input_bytes,
    view_result_bytes,
    view_output_bytes,
    max_reorg_blocks,
    // Record
    record_refresh_ms,
    record_stale_ms,
    record_page_blocks,
    record_pages_per_refresh,
    record_overlap_blocks,
    record_checkpoints,
    log_batch_addresses,
    immutable_cache_entries,
    immutable_cache_bytes,
    // Solana
    solana_active_round_ms,
    solana_notice_ms,
    solana_record_refresh_ms,
    solana_rescan_ms,
    solana_rent_poll_rounds,
    solana_history_transactions,
);

/// The longest `record_stale_ms`, and the longest `solana_rescan_ms`: a day.
const DAY_MS: u64 = 86_400_000;

/// The highest `keeper_balance_floor_micro`: a million of the chain's coin.
const MOST_FLOOR_MICRO: u64 = 1_000_000_000_000;

/// The `[rpc]` tables of one config file: the defaults, the process-wide
/// polls, and each chain's partial overrides.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RpcPolicies {
    #[serde(default)]
    pub defaults: RpcPolicy,
    #[serde(default)]
    pub service: ServiceRpcPolicy,
    #[serde(default)]
    pub chains: BTreeMap<ChainKey, RpcOverrides>,
}

/// The checked-in `config/chains.toml`, parsed once.
fn checked_in() -> &'static RpcPolicies {
    static POLICIES: OnceLock<RpcPolicies> = OnceLock::new();
    POLICIES.get_or_init(|| {
        // Every table is required here, so reading the checked-in file never
        // falls back on its own defaults.
        #[derive(Deserialize)]
        struct File {
            rpc: Rpc,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Rpc {
            defaults: RpcPolicy,
            service: ServiceRpcPolicy,
            chains: BTreeMap<ChainKey, RpcOverrides>,
        }
        let rpc = toml::from_str::<File>(include_str!("../../../config/chains.toml"))
            .expect("the checked-in RPC settings parse")
            .rpc;
        RpcPolicies {
            defaults: rpc.defaults,
            service: rpc.service,
            chains: rpc.chains,
        }
    })
}

impl Default for RpcPolicy {
    /// The checked-in `[rpc.defaults]`: there is no second set of numbers in
    /// the code.
    fn default() -> Self {
        checked_in().defaults.clone()
    }
}

impl Default for RpcPolicies {
    /// The checked-in defaults and service polls, with no chain's overrides:
    /// what a file without `[rpc]` tables gets, since the checked-in
    /// overrides name the checked-in chains.
    fn default() -> Self {
        Self {
            defaults: RpcPolicy::default(),
            service: ServiceRpcPolicy::default(),
            chains: BTreeMap::new(),
        }
    }
}

impl RpcPolicies {
    /// The checked-in settings, every chain's overrides included: the
    /// established values a configuration is judged against at startup.
    pub fn established() -> &'static RpcPolicies {
        checked_in()
    }

    /// The defaults with only this chain's overrides applied.
    pub fn for_chain(&self, key: &ChainKey) -> RpcPolicy {
        match self.chains.get(key) {
            Some(overrides) => overrides.apply(self.defaults.clone()),
            None => self.defaults.clone(),
        }
    }
}

impl RpcPolicy {
    /// Check every range and every bound between two settings. `scope` names
    /// whose settings these are in the error: a chain key, or "defaults".
    ///
    /// Every round and poll has a floor of at least 250 ms, except
    /// `receipt_poll_ms` (100 ms); the rate-limit backoff's first wait (1 ms)
    /// is a backoff, not a poll. Whether a vault's timers can be met is not
    /// checked here: see [`RpcPolicy::two_observations_fit`] and its siblings.
    pub fn validate(&self, scope: &str) -> Result<(), ConfigError> {
        let refuse = |message: String| {
            Err(ConfigError::new(format!(
                "RPC settings for {scope}: {message}"
            )))
        };
        let range = |field: &str, value: u64, min: u64, max: u64| {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                refuse(format!("{field}={value}, expected {min}..={max}"))
            }
        };
        let at_least = |field: &str, value: u64, floor: &str, floor_value: u64| {
            if value >= floor_value {
                Ok(())
            } else {
                refuse(format!("{field}={value} is below {floor}={floor_value}"))
            }
        };
        let at_most = |field: &str, value: u64, ceiling: &str, ceiling_value: u64| {
            if value <= ceiling_value {
                Ok(())
            } else {
                refuse(format!(
                    "{field}={value} is above {ceiling}={ceiling_value}"
                ))
            }
        };

        // Keeper (EVM). A field another is bounded by is checked first, so a
        // refusal names the field that is out of its own range.
        range(
            "keeper_active_round_ms",
            self.keeper_active_round_ms,
            250,
            2_000,
        )?;
        at_least(
            "keeper_notice_ms",
            self.keeper_notice_ms,
            "keeper_active_round_ms",
            self.keeper_active_round_ms,
        )?;
        range(
            "keeper_notice_ms",
            self.keeper_notice_ms,
            self.keeper_active_round_ms,
            60_000,
        )?;
        at_least(
            "keeper_log_scan_ms",
            self.keeper_log_scan_ms,
            "keeper_notice_ms",
            self.keeper_notice_ms,
        )?;
        range(
            "keeper_log_scan_ms",
            self.keeper_log_scan_ms,
            self.keeper_notice_ms,
            3_600_000,
        )?;
        range("receipt_poll_ms", self.receipt_poll_ms, 100, 7_000)?;
        range(
            "keeper_simulation_initial_ms",
            self.keeper_simulation_initial_ms,
            1_000,
            5_000,
        )?;
        range(
            "keeper_simulation_max_ms",
            self.keeper_simulation_max_ms,
            1_000,
            300_000,
        )?;
        at_least(
            "keeper_simulation_max_ms",
            self.keeper_simulation_max_ms,
            "keeper_simulation_initial_ms",
            self.keeper_simulation_initial_ms,
        )?;
        range(
            "keeper_simulation_backoff_factor",
            self.keeper_simulation_backoff_factor,
            1,
            2,
        )?;
        range(
            "keeper_rate_limit_initial_ms",
            self.keeper_rate_limit_initial_ms,
            1,
            5_000,
        )?;
        range(
            "keeper_rate_limit_max_ms",
            self.keeper_rate_limit_max_ms,
            1,
            120_000,
        )?;
        at_least(
            "keeper_rate_limit_max_ms",
            self.keeper_rate_limit_max_ms,
            "keeper_rate_limit_initial_ms",
            self.keeper_rate_limit_initial_ms,
        )?;
        range(
            "keeper_rate_limit_backoff_factor",
            self.keeper_rate_limit_backoff_factor,
            1,
            2,
        )?;
        range("key_poll_ms", self.key_poll_ms, 250, 2_000)?;
        range("key_look_ms", self.key_look_ms, 250, 15_000)?;
        range(
            "keeper_balance_floor_micro",
            self.keeper_balance_floor_micro,
            0,
            MOST_FLOOR_MICRO,
        )?;

        // Chain clock and observation.
        range("head_lag_max_ms", self.head_lag_max_ms, 500, 30_000)?;
        range("head_ahead_max_ms", self.head_ahead_max_ms, 0, 10_000)?;
        range(
            "clock_settle_observations",
            self.clock_settle_observations,
            1,
            20,
        )?;
        range("read_budget_ms", self.read_budget_ms, 100, 10_000)?;
        range("view_batch_reads", self.view_batch_reads, 1, 512)?;
        range(
            "view_outer_gas",
            self.view_outer_gas,
            1_000_000,
            100_000_000,
        )?;
        range("view_inner_gas", self.view_inner_gas, 21_000, 1_000_000)?;
        range(
            "view_batch_input_bytes",
            self.view_batch_input_bytes,
            1_024,
            1_048_576,
        )?;
        range("view_result_bytes", self.view_result_bytes, 32, 16_384)?;
        range(
            "view_output_bytes",
            self.view_output_bytes,
            65_536,
            1_048_576,
        )?;
        range("max_reorg_blocks", self.max_reorg_blocks, 1, 256)?;

        // Record. Both refresh fields come before the staleness they bound.
        range("record_refresh_ms", self.record_refresh_ms, 2_000, 300_000)?;
        range(
            "solana_record_refresh_ms",
            self.solana_record_refresh_ms,
            2_000,
            300_000,
        )?;
        let (refresh, refresh_value) = if self.solana_record_refresh_ms > self.record_refresh_ms {
            ("solana_record_refresh_ms", self.solana_record_refresh_ms)
        } else {
            ("record_refresh_ms", self.record_refresh_ms)
        };
        let stale_floor = refresh_value
            .saturating_mul(2)
            .saturating_add(self.read_budget_ms);
        if self.record_stale_ms < stale_floor {
            return refuse(format!(
                "record_stale_ms={} is below 2 * {refresh}={refresh_value} + read_budget_ms={} = {stale_floor}",
                self.record_stale_ms, self.read_budget_ms
            ));
        }
        range("record_stale_ms", self.record_stale_ms, stale_floor, DAY_MS)?;
        range("record_page_blocks", self.record_page_blocks, 1, 10_000)?;
        range(
            "record_pages_per_refresh",
            self.record_pages_per_refresh,
            1,
            100,
        )?;
        at_most(
            "record_overlap_blocks",
            self.record_overlap_blocks,
            "max_reorg_blocks",
            self.max_reorg_blocks,
        )?;
        range("record_checkpoints", self.record_checkpoints, 2, 256)?;
        range("log_batch_addresses", self.log_batch_addresses, 1, 1_024)?;
        range(
            "immutable_cache_entries",
            self.immutable_cache_entries,
            0,
            65_536,
        )?;
        range(
            "immutable_cache_bytes",
            self.immutable_cache_bytes,
            0,
            67_108_864,
        )?;

        // Solana.
        range(
            "solana_active_round_ms",
            self.solana_active_round_ms,
            250,
            2_000,
        )?;
        at_least(
            "solana_notice_ms",
            self.solana_notice_ms,
            "solana_active_round_ms",
            self.solana_active_round_ms,
        )?;
        range(
            "solana_notice_ms",
            self.solana_notice_ms,
            self.solana_active_round_ms,
            60_000,
        )?;
        range("solana_rescan_ms", self.solana_rescan_ms, 0, DAY_MS)?;
        range(
            "solana_rent_poll_rounds",
            self.solana_rent_poll_rounds,
            1,
            30,
        )?;
        range(
            "solana_history_transactions",
            self.solana_history_transactions,
            1,
            4_096,
        )?;
        Ok(())
    }
}

/// A vault's slot timers, in whole seconds as the contracts keep them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotTimers {
    /// How long before a slot its requests are fixed (`slotLead`).
    pub slot_lead_s: u64,
    /// How long a slot stays open (`slotWindow`).
    pub slot_window_s: u64,
}

impl RpcPolicy {
    /// Two observations, each an active round and a read, fit in a slot's
    /// window of opportunity less one block:
    /// `2 * (keeper_active_round_ms + read_budget_ms) <= slotLead + slotWindow - block interval`.
    pub fn two_observations_fit(
        &self,
        timers: &SlotTimers,
        block_interval_ms: u64,
    ) -> Result<(), String> {
        let needed = self
            .keeper_active_round_ms
            .saturating_add(self.read_budget_ms)
            .saturating_mul(2);
        let room = timers
            .slot_lead_s
            .saturating_mul(1_000)
            .saturating_add(timers.slot_window_s.saturating_mul(1_000))
            .saturating_sub(block_interval_ms);
        if needed <= room {
            return Ok(());
        }
        Err(format!(
            "two observations need 2 * (keeper_active_round_ms={} + read_budget_ms={}) = {needed}ms, \
             but slotLead={}s + slotWindow={}s - block interval {block_interval_ms}ms leaves {room}ms",
            self.keeper_active_round_ms,
            self.read_budget_ms,
            timers.slot_lead_s,
            timers.slot_window_s,
        ))
    }

    /// A head as old as `head_lag_max_ms` still shows every request for a
    /// slot at its decision point: `head_lag_max_ms + block interval < slotLead`.
    pub fn bounded_head_sees_requests(
        &self,
        timers: &SlotTimers,
        block_interval_ms: u64,
    ) -> Result<(), String> {
        let age = self.head_lag_max_ms.saturating_add(block_interval_ms);
        let lead = timers.slot_lead_s.saturating_mul(1_000);
        if age < lead {
            return Ok(());
        }
        Err(format!(
            "head_lag_max_ms={} + block interval {block_interval_ms}ms = {age}ms is not below slotLead={}s",
            self.head_lag_max_ms, timers.slot_lead_s,
        ))
    }

    /// An outsider's transaction is noticed and an ask answered before a
    /// halt's grace and the last orders run out:
    /// `keeper_notice_ms + ask timeout < haltGrace` and `< last orders`.
    pub fn notice_before_deadlines(
        &self,
        ask_timeout_ms: u64,
        halt_grace_ms: u64,
        last_orders_ms: u64,
    ) -> Result<(), String> {
        let needed = self.keeper_notice_ms.saturating_add(ask_timeout_ms);
        for (deadline, value) in [
            ("haltGrace", halt_grace_ms),
            ("last orders", last_orders_ms),
        ] {
            if needed >= value {
                return Err(format!(
                    "keeper_notice_ms={} + ask timeout {ask_timeout_ms}ms = {needed}ms is not below {deadline}={value}ms",
                    self.keeper_notice_ms,
                ));
            }
        }
        Ok(())
    }
}

/// What a startup check makes of a vault or factory's timers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// The configured settings meet the timer.
    Fits,
    /// Even the established settings miss the timer: a warning, and the
    /// vault is still served as well as it can be.
    BestEffort(String),
    /// The established settings meet the timer and the configured ones do
    /// not: the configuration is refused.
    Refused(String),
}

/// Judge `configured` by `check`, against what `established` (the checked-in
/// settings for the same chain) would make of the same timers. Only a timer
/// the established settings meet and the configured ones miss is refused;
/// one that even the established settings miss is served best-effort, as it
/// was before these settings existed. `subject` names the vault or factory.
pub fn judge(
    configured: &RpcPolicy,
    established: &RpcPolicy,
    subject: &str,
    check: impl Fn(&RpcPolicy) -> Result<(), String>,
) -> Fit {
    let Err(shortfall) = check(configured) else {
        return Fit::Fits;
    };
    match check(established) {
        Ok(()) => Fit::Refused(format!(
            "{subject}: {shortfall}; the established RPC settings meet this timer, so the configured ones are refused"
        )),
        Err(established) => Fit::BestEffort(format!(
            "{subject}: {shortfall}; the established RPC settings miss this timer too ({established}), so it is served best-effort"
        )),
    }
}

/// Process-wide polls, which no chain overrides.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceRpcPolicy {
    pub indexer_queue_poll_ms: u64,
    pub indexer_discovery_poll_ms: u64,
}

impl Default for ServiceRpcPolicy {
    /// The checked-in `[rpc.service]`.
    fn default() -> Self {
        checked_in().service.clone()
    }
}

impl ServiceRpcPolicy {
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, value) in [
            ("indexer_queue_poll_ms", self.indexer_queue_poll_ms),
            ("indexer_discovery_poll_ms", self.indexer_discovery_poll_ms),
        ] {
            if !(1_000..=2_000).contains(&value) {
                return Err(ConfigError::new(format!(
                    "RPC settings for service: {field}={value}, expected 1000..=2000"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ChainConfigs;

    const SCOPE: &str = "evm:84532";

    fn refusal(policy: &RpcPolicy) -> String {
        policy
            .validate(SCOPE)
            .expect_err("these settings are refused")
            .to_string()
    }

    fn with(changes: &[(&str, u64)]) -> RpcPolicy {
        let mut policy = RpcPolicy::default();
        for (field, value) in changes {
            policy.set(field, *value);
        }
        policy
    }

    fn checked_in_configs() -> ChainConfigs {
        ChainConfigs::from_toml(include_str!("../../../config/chains.toml"), |_| None).unwrap()
    }

    fn key(text: &str) -> ChainKey {
        text.parse().unwrap()
    }

    /// One field's name, its accepted minimum and maximum, and the settings
    /// it needs beside it for both ends to be reachable.
    type Range = (&'static str, u64, u64, &'static [(&'static str, u64)]);

    /// Each field's accepted range, and the settings it needs beside it for
    /// both ends to be reachable.
    const RANGES: &[Range] = &[
        (
            "keeper_active_round_ms",
            250,
            2_000,
            &[("keeper_notice_ms", 2_000)],
        ),
        (
            "keeper_notice_ms",
            250,
            60_000,
            &[
                ("keeper_active_round_ms", 250),
                ("keeper_log_scan_ms", 60_000),
            ],
        ),
        (
            "keeper_log_scan_ms",
            250,
            3_600_000,
            &[("keeper_active_round_ms", 250), ("keeper_notice_ms", 250)],
        ),
        ("receipt_poll_ms", 100, 7_000, &[]),
        (
            "keeper_simulation_initial_ms",
            1_000,
            5_000,
            &[("keeper_simulation_max_ms", 5_000)],
        ),
        (
            "keeper_simulation_max_ms",
            1_000,
            300_000,
            &[("keeper_simulation_initial_ms", 1_000)],
        ),
        ("keeper_simulation_backoff_factor", 1, 2, &[]),
        (
            "keeper_rate_limit_initial_ms",
            1,
            5_000,
            &[("keeper_rate_limit_max_ms", 5_000)],
        ),
        (
            "keeper_rate_limit_max_ms",
            1,
            120_000,
            &[("keeper_rate_limit_initial_ms", 1)],
        ),
        ("keeper_rate_limit_backoff_factor", 1, 2, &[]),
        ("key_poll_ms", 250, 2_000, &[]),
        ("key_look_ms", 250, 15_000, &[]),
        ("keeper_balance_floor_micro", 0, MOST_FLOOR_MICRO, &[]),
        ("head_lag_max_ms", 500, 30_000, &[]),
        ("head_ahead_max_ms", 0, 10_000, &[]),
        ("clock_settle_observations", 1, 20, &[]),
        ("read_budget_ms", 100, 10_000, &[]),
        ("view_batch_reads", 1, 512, &[]),
        ("view_outer_gas", 1_000_000, 100_000_000, &[]),
        ("view_inner_gas", 21_000, 1_000_000, &[]),
        ("view_batch_input_bytes", 1_024, 1_048_576, &[]),
        ("view_result_bytes", 32, 16_384, &[]),
        ("view_output_bytes", 65_536, 1_048_576, &[]),
        ("max_reorg_blocks", 1, 256, &[("record_overlap_blocks", 0)]),
        (
            "record_refresh_ms",
            2_000,
            300_000,
            &[
                ("solana_record_refresh_ms", 2_000),
                ("record_stale_ms", DAY_MS),
            ],
        ),
        (
            "record_stale_ms",
            2 * 2_000 + 100,
            DAY_MS,
            &[
                ("record_refresh_ms", 2_000),
                ("solana_record_refresh_ms", 2_000),
                ("read_budget_ms", 100),
            ],
        ),
        ("record_page_blocks", 1, 10_000, &[]),
        ("record_pages_per_refresh", 1, 100, &[]),
        (
            "record_overlap_blocks",
            0,
            256,
            &[("max_reorg_blocks", 256)],
        ),
        ("record_checkpoints", 2, 256, &[]),
        ("log_batch_addresses", 1, 1_024, &[]),
        ("immutable_cache_entries", 0, 65_536, &[]),
        ("immutable_cache_bytes", 0, 67_108_864, &[]),
        (
            "solana_active_round_ms",
            250,
            2_000,
            &[("solana_notice_ms", 2_000)],
        ),
        (
            "solana_notice_ms",
            250,
            60_000,
            &[("solana_active_round_ms", 250)],
        ),
        (
            "solana_record_refresh_ms",
            2_000,
            300_000,
            &[("record_refresh_ms", 2_000), ("record_stale_ms", DAY_MS)],
        ),
        ("solana_rescan_ms", 0, DAY_MS, &[]),
        ("solana_rent_poll_rounds", 1, 30, &[]),
        ("solana_history_transactions", 1, 4_096, &[]),
    ];

    #[test]
    fn every_field_accepts_its_minimum_and_maximum_and_refuses_one_beyond_by_name() {
        let ranged: Vec<&str> = RANGES.iter().map(|(field, ..)| *field).collect();
        assert_eq!(ranged, RpcPolicy::FIELDS, "every field has a range here");
        for (field, min, max, beside) in RANGES {
            let context = with(beside);
            context.validate(SCOPE).unwrap();
            for value in [*min, *max] {
                let mut policy = context.clone();
                policy.set(field, value);
                policy
                    .validate(SCOPE)
                    .unwrap_or_else(|error| panic!("{field}={value} is accepted: {error}"));
            }
            let mut refused = vec![max + 1];
            if *min > 0 {
                refused.push(min - 1);
            }
            for value in refused {
                let mut policy = context.clone();
                policy.set(field, value);
                let error = refusal(&policy);
                assert!(
                    error.starts_with(&format!("RPC settings for {SCOPE}: {field}={value}")),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn keeper_notice_ms_may_equal_keeper_active_round_ms_but_not_fall_below_it() {
        for active in [250, 1_000, 2_000] {
            with(&[
                ("keeper_active_round_ms", active),
                ("keeper_notice_ms", active),
            ])
            .validate(SCOPE)
            .unwrap();
            let error = refusal(&with(&[
                ("keeper_active_round_ms", active),
                ("keeper_notice_ms", active - 1),
            ]));
            assert_eq!(
                error,
                format!(
                    "RPC settings for {SCOPE}: keeper_notice_ms={} is below keeper_active_round_ms={active}",
                    active - 1
                )
            );
        }
    }

    #[test]
    fn keeper_log_scan_ms_may_equal_keeper_notice_ms_but_not_fall_below_it() {
        for notice in [2_000, 5_000, 60_000] {
            with(&[("keeper_notice_ms", notice), ("keeper_log_scan_ms", notice)])
                .validate(SCOPE)
                .unwrap();
            let error = refusal(&with(&[
                ("keeper_notice_ms", notice),
                ("keeper_log_scan_ms", notice - 1),
            ]));
            assert!(
                error.contains(&format!("keeper_log_scan_ms={}", notice - 1))
                    && error.contains(&format!("keeper_notice_ms={notice}")),
                "{error}"
            );
        }
    }

    #[test]
    fn solana_notice_ms_may_equal_solana_active_round_ms_but_not_fall_below_it() {
        for active in [250, 2_000] {
            with(&[
                ("solana_active_round_ms", active),
                ("solana_notice_ms", active),
            ])
            .validate(SCOPE)
            .unwrap();
            let error = refusal(&with(&[
                ("solana_active_round_ms", active),
                ("solana_notice_ms", active - 1),
            ]));
            assert!(
                error.contains(&format!("solana_notice_ms={}", active - 1))
                    && error.contains(&format!("solana_active_round_ms={active}")),
                "{error}"
            );
        }
    }

    #[test]
    fn a_backoff_maximum_may_equal_its_first_wait_but_not_fall_below_it() {
        for (initial, max, first) in [
            (
                "keeper_simulation_initial_ms",
                "keeper_simulation_max_ms",
                1_000,
            ),
            (
                "keeper_simulation_initial_ms",
                "keeper_simulation_max_ms",
                5_000,
            ),
            (
                "keeper_rate_limit_initial_ms",
                "keeper_rate_limit_max_ms",
                2,
            ),
            (
                "keeper_rate_limit_initial_ms",
                "keeper_rate_limit_max_ms",
                5_000,
            ),
        ] {
            with(&[(initial, first), (max, first)])
                .validate(SCOPE)
                .unwrap();
            let error = refusal(&with(&[(initial, first), (max, first - 1)]));
            let expected = if first - 1 < 1_000 && max == "keeper_simulation_max_ms" {
                // Below its own range first.
                format!("{max}={}, expected 1000..=300000", first - 1)
            } else {
                format!("{max}={} is below {initial}={first}", first - 1)
            };
            assert!(error.ends_with(&expected), "{error}");
        }
    }

    #[test]
    fn record_stale_ms_covers_twice_the_slower_refresh_and_a_read_budget_exactly() {
        for (record, solana, budget, slower) in [
            (30_000, 30_000, 1_000, "record_refresh_ms"),
            (40_000, 30_000, 1_000, "record_refresh_ms"),
            (30_000, 50_000, 2_000, "solana_record_refresh_ms"),
            (2_000, 300_000, 100, "solana_record_refresh_ms"),
            (300_000, 2_000, 10_000, "record_refresh_ms"),
        ] {
            let floor = 2 * record.max(solana) + budget;
            let settings = |stale| {
                with(&[
                    ("record_refresh_ms", record),
                    ("solana_record_refresh_ms", solana),
                    ("read_budget_ms", budget),
                    ("record_stale_ms", stale),
                ])
            };
            settings(floor).validate(SCOPE).unwrap();
            let error = refusal(&settings(floor - 1));
            assert_eq!(
                error,
                format!(
                    "RPC settings for {SCOPE}: record_stale_ms={} is below 2 * {slower}={} + read_budget_ms={budget} = {floor}",
                    floor - 1,
                    record.max(solana)
                )
            );
        }
    }

    #[test]
    fn record_overlap_blocks_may_equal_max_reorg_blocks_but_not_exceed_it() {
        for reorg in [1, 32, 256] {
            with(&[
                ("max_reorg_blocks", reorg),
                ("record_overlap_blocks", reorg),
            ])
            .validate(SCOPE)
            .unwrap();
            let error = refusal(&with(&[
                ("max_reorg_blocks", reorg),
                ("record_overlap_blocks", reorg + 1),
            ]));
            assert_eq!(
                error,
                format!(
                    "RPC settings for {SCOPE}: record_overlap_blocks={} is above max_reorg_blocks={reorg}",
                    reorg + 1
                )
            );
        }
    }

    /// The demo timers: slotLead 5 s, slotWindow 3 s.
    const DEMO: SlotTimers = SlotTimers {
        slot_lead_s: 5,
        slot_window_s: 3,
    };

    #[test]
    fn two_observations_fit_the_demo_window_exactly_and_one_millisecond_more_is_refused() {
        let defaults = RpcPolicy::default();
        assert_eq!(
            (defaults.keeper_active_round_ms, defaults.read_budget_ms),
            (2_000, 1_000)
        );
        // 2 * (2000 + 1000) = 5000 + 3000 - 2000.
        defaults.two_observations_fit(&DEMO, 2_000).unwrap();
        let error = defaults.two_observations_fit(&DEMO, 2_001).unwrap_err();
        assert_eq!(
            error,
            "two observations need 2 * (keeper_active_round_ms=2000 + read_budget_ms=1000) = 6000ms, \
             but slotLead=5s + slotWindow=3s - block interval 2001ms leaves 5999ms"
        );
        let slower = with(&[("read_budget_ms", 1_001)]);
        let error = slower.two_observations_fit(&DEMO, 2_000).unwrap_err();
        assert!(
            error.contains("read_budget_ms=1001") && error.contains("= 6002ms"),
            "{error}"
        );
        // A block interval longer than the whole window leaves nothing.
        let error = defaults.two_observations_fit(&DEMO, u64::MAX).unwrap_err();
        assert!(error.ends_with("leaves 0ms"), "{error}");
    }

    #[test]
    fn a_bounded_head_must_be_younger_than_the_slot_lead_by_a_block() {
        let inside = with(&[("head_lag_max_ms", 2_999)]);
        inside.bounded_head_sees_requests(&DEMO, 2_000).unwrap();
        let outside = with(&[("head_lag_max_ms", 3_000)]);
        let error = outside
            .bounded_head_sees_requests(&DEMO, 2_000)
            .unwrap_err();
        assert_eq!(
            error,
            "head_lag_max_ms=3000 + block interval 2000ms = 5000ms is not below slotLead=5s"
        );
        inside.bounded_head_sees_requests(&DEMO, 2_001).unwrap_err();
    }

    #[test]
    fn notice_and_an_ask_end_before_halt_grace_and_last_orders() {
        let defaults = RpcPolicy::default();
        let ask = 30_000;
        let needed = defaults.keeper_notice_ms + ask;
        defaults
            .notice_before_deadlines(ask, needed + 1, needed + 1)
            .unwrap();
        let error = defaults
            .notice_before_deadlines(ask, needed, needed + 1)
            .unwrap_err();
        assert_eq!(
            error,
            format!(
                "keeper_notice_ms=5000 + ask timeout 30000ms = {needed}ms is not below haltGrace={needed}ms"
            )
        );
        let error = defaults
            .notice_before_deadlines(ask, needed + 1, needed)
            .unwrap_err();
        assert!(
            error.ends_with(&format!("is not below last orders={needed}ms")),
            "{error}"
        );
        // The contract minimums are far beyond any accepted notice.
        with(&[("keeper_notice_ms", 60_000)])
            .notice_before_deadlines(ask, 600_000, 900_000)
            .unwrap();
    }

    #[test]
    fn judge_fits_when_the_configured_settings_meet_the_timer() {
        let established = RpcPolicy::default();
        let configured = with(&[("keeper_active_round_ms", 1_000)]);
        assert_eq!(
            judge(&configured, &established, "vault 0x01", |policy| {
                policy.two_observations_fit(&DEMO, 2_000)
            }),
            Fit::Fits
        );
    }

    #[test]
    fn judge_refuses_only_what_the_established_settings_meet_and_the_configured_miss() {
        let established = RpcPolicy::default();
        let configured = with(&[("read_budget_ms", 4_000)]);
        let Fit::Refused(message) = judge(&configured, &established, "vault 0x01", |policy| {
            policy.two_observations_fit(&DEMO, 2_000)
        }) else {
            panic!("refused");
        };
        assert!(
            message.starts_with("vault 0x01: ")
                && message.contains("read_budget_ms=4000")
                && message.contains("slotLead=5s"),
            "{message}"
        );
    }

    #[test]
    fn judge_serves_best_effort_what_even_the_established_settings_miss() {
        let established = RpcPolicy::default();
        let configured = with(&[("read_budget_ms", 4_000)]);
        let short = SlotTimers {
            slot_lead_s: 5,
            slot_window_s: 2,
        };
        let Fit::BestEffort(message) = judge(&configured, &established, "factory 0x02", |policy| {
            policy.two_observations_fit(&short, 2_000)
        }) else {
            panic!("best-effort");
        };
        assert!(
            message.starts_with("factory 0x02: ")
                && message.contains("read_budget_ms=4000")
                && message.contains("read_budget_ms=1000")
                && message.contains("slotWindow=2s"),
            "{message}"
        );
        // A configuration that meets a timer is never worse off for the
        // established settings missing it.
        assert_eq!(
            judge(&established, &configured, "factory 0x02", |policy| {
                policy.two_observations_fit(&DEMO, 2_000)
            }),
            Fit::Fits
        );
    }

    #[test]
    fn the_checked_in_defaults_validate_and_notice_defaults_to_five_seconds() {
        let defaults = RpcPolicy::default();
        defaults.validate("defaults").unwrap();
        ServiceRpcPolicy::default().validate().unwrap();
        assert_eq!(defaults.keeper_notice_ms, 5_000);
        assert_eq!(defaults.solana_notice_ms, 5_000);
        assert_eq!(defaults.keeper_active_round_ms, 2_000);
        assert_eq!(defaults.receipt_poll_ms, 2_000);
        assert_eq!(defaults.head_lag_max_ms, 3_000);
        assert_eq!(defaults.head_ahead_max_ms, 2_000);
        assert_eq!(defaults.record_stale_ms, 180_000);
        assert_eq!(RpcPolicies::established().defaults, defaults);
    }

    #[test]
    fn each_checked_in_chain_s_settings_are_the_defaults_with_its_documented_overrides() {
        let configs = checked_in_configs();
        let defaults = RpcPolicy::default();
        let expected: &[(&str, &[(&str, u64)])] = &[
            ("evm:31337", &[("head_lag_max_ms", 4_000)]),
            (
                "evm:84532",
                &[
                    ("head_lag_max_ms", 4_000),
                    ("keeper_balance_floor_micro", 2_000),
                ],
            ),
            (
                "evm:8453",
                &[
                    ("head_lag_max_ms", 4_000),
                    ("keeper_balance_floor_micro", 25_000),
                ],
            ),
            ("evm:31338", &[("receipt_poll_ms", 1_000)]),
            (
                "evm:998",
                &[
                    ("receipt_poll_ms", 1_000),
                    ("keeper_balance_floor_micro", 200_000),
                ],
            ),
            (
                "evm:999",
                &[
                    ("receipt_poll_ms", 1_000),
                    ("keeper_balance_floor_micro", 200_000),
                ],
            ),
            ("evm:31339", &[("receipt_poll_ms", 500)]),
            (
                "evm:46630",
                &[
                    ("receipt_poll_ms", 500),
                    ("keeper_balance_floor_micro", 2_000),
                ],
            ),
            (
                "evm:4663",
                &[
                    ("receipt_poll_ms", 500),
                    ("keeper_balance_floor_micro", 70_000),
                ],
            ),
            (
                "solana:localnet",
                &[("head_lag_max_ms", 10_000), ("head_ahead_max_ms", 5_000)],
            ),
            (
                "solana:devnet",
                &[
                    ("head_lag_max_ms", 10_000),
                    ("head_ahead_max_ms", 5_000),
                    ("keeper_balance_floor_micro", 200_000),
                ],
            ),
            (
                "solana:mainnet-beta",
                &[
                    ("head_lag_max_ms", 10_000),
                    ("head_ahead_max_ms", 5_000),
                    ("keeper_balance_floor_micro", 500_000),
                ],
            ),
        ];
        assert_eq!(configs.iter().count(), expected.len());
        for (chain, overrides) in expected {
            let policy = configs.rpc_policy(&key(chain));
            policy.validate(chain).unwrap();
            for field in RpcPolicy::FIELDS {
                let want = overrides
                    .iter()
                    .find(|(name, _)| name == field)
                    .map_or(defaults.get(field).unwrap(), |(_, value)| *value);
                assert_eq!(policy.get(field), Some(want), "{chain} {field}");
            }
            assert_eq!(RpcPolicies::established().for_chain(&key(chain)), policy);
        }
        assert_eq!(configs.service_rpc_policy(), &ServiceRpcPolicy::default());
    }

    #[test]
    fn a_partial_override_keeps_every_other_value() {
        let defaults = RpcPolicy::default();
        let overrides: RpcOverrides =
            toml::from_str("keeper_notice_ms = 10000\nrecord_refresh_ms = 40000").unwrap();
        let policy = overrides.apply(defaults.clone());
        for field in RpcPolicy::FIELDS {
            let want = match *field {
                "keeper_notice_ms" => 10_000,
                "record_refresh_ms" => 40_000,
                _ => defaults.get(field).unwrap(),
            };
            assert_eq!(policy.get(field), Some(want), "{field}");
        }
        assert_eq!(RpcOverrides::default().apply(defaults.clone()), defaults);
    }

    #[test]
    fn unknown_fields_are_refused_in_defaults_overrides_and_service() {
        let mut complete = String::new();
        for field in RpcPolicy::FIELDS {
            complete += &format!("{field} = {}\n", RpcPolicy::default().get(field).unwrap());
        }
        assert_eq!(
            toml::from_str::<RpcPolicy>(&complete).unwrap(),
            RpcPolicy::default()
        );
        let error = toml::from_str::<RpcPolicy>(&format!("{complete}head_poll_ms = 2000\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("head_poll_ms"), "{error}");
        let error = toml::from_str::<RpcOverrides>("event_scan_ms = 2000")
            .unwrap_err()
            .to_string();
        assert!(error.contains("event_scan_ms"), "{error}");
        let error = toml::from_str::<ServiceRpcPolicy>(
            "indexer_queue_poll_ms = 2000\nindexer_discovery_poll_ms = 2000\nindexer_idle_max_ms = 1",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("indexer_idle_max_ms"), "{error}");
        let error = toml::from_str::<RpcPolicies>("[quiet]\nquiet_probe_retry_ms = 1")
            .unwrap_err()
            .to_string();
        assert!(error.contains("quiet"), "{error}");
        // A missing default is not filled in from elsewhere.
        assert!(toml::from_str::<RpcPolicy>("keeper_notice_ms = 5000").is_err());
    }

    #[test]
    fn service_polls_accept_one_to_two_seconds_and_name_what_they_refuse() {
        let defaults = ServiceRpcPolicy::default();
        assert_eq!(defaults.indexer_queue_poll_ms, 2_000);
        assert_eq!(defaults.indexer_discovery_poll_ms, 2_000);
        for queue in [true, false] {
            for value in [1_000, 2_000] {
                let mut policy = defaults.clone();
                if queue {
                    policy.indexer_queue_poll_ms = value;
                } else {
                    policy.indexer_discovery_poll_ms = value;
                }
                policy.validate().unwrap();
            }
            for value in [999, 2_001] {
                let mut policy = defaults.clone();
                let field = if queue {
                    policy.indexer_queue_poll_ms = value;
                    "indexer_queue_poll_ms"
                } else {
                    policy.indexer_discovery_poll_ms = value;
                    "indexer_discovery_poll_ms"
                };
                assert_eq!(
                    policy.validate().unwrap_err().to_string(),
                    format!("RPC settings for service: {field}={value}, expected 1000..=2000")
                );
            }
        }
    }

    const ONE_CHAIN: &str = r#"
        [chains."evm:31337"]
        label = "Base (local)"
        rpc = "http://127.0.0.1:19100"
        dollar = "USDC"

        [networks.local]
        base = "evm:31337"
    "#;

    #[test]
    fn rpc_settings_for_a_chain_not_configured_are_refused() {
        let text = format!("{ONE_CHAIN}\n[rpc.chains.\"evm:1\"]\nreceipt_poll_ms = 1000\n");
        let error = ChainConfigs::from_toml(&text, |_| None)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "RPC settings name evm:1, which has no chain configuration"
        );
    }

    #[test]
    fn a_file_without_rpc_tables_uses_the_checked_in_defaults_and_no_overrides() {
        let configs = ChainConfigs::from_toml(ONE_CHAIN, |_| None).unwrap();
        assert_eq!(configs.rpc_policy(&key("evm:31337")), RpcPolicy::default());
        assert_eq!(configs.service_rpc_policy(), &ServiceRpcPolicy::default());
    }

    #[test]
    fn a_file_s_invalid_settings_are_refused_naming_their_scope() {
        let text = format!("{ONE_CHAIN}\n[rpc.chains.\"evm:31337\"]\nkeeper_notice_ms = 1999\n");
        assert_eq!(
            ChainConfigs::from_toml(&text, |_| None)
                .unwrap_err()
                .to_string(),
            "RPC settings for evm:31337: keeper_notice_ms=1999 is below keeper_active_round_ms=2000"
        );

        let mut defaults = String::from("[rpc.defaults]\n");
        for field in RpcPolicy::FIELDS {
            let value = match *field {
                "receipt_poll_ms" => 7_001,
                _ => RpcPolicy::default().get(field).unwrap(),
            };
            defaults += &format!("{field} = {value}\n");
        }
        assert_eq!(
            ChainConfigs::from_toml(&format!("{ONE_CHAIN}\n{defaults}"), |_| None)
                .unwrap_err()
                .to_string(),
            "RPC settings for defaults: receipt_poll_ms=7001, expected 100..=7000"
        );

        let text = format!(
            "{ONE_CHAIN}\n[rpc.service]\nindexer_queue_poll_ms = 999\nindexer_discovery_poll_ms = 2000\n"
        );
        assert_eq!(
            ChainConfigs::from_toml(&text, |_| None)
                .unwrap_err()
                .to_string(),
            "RPC settings for service: indexer_queue_poll_ms=999, expected 1000..=2000"
        );

        let text = format!("{ONE_CHAIN}\n[rpc.chains.\"evm:31337\"]\nhead_poll_ms = 2000\n");
        let error = ChainConfigs::from_toml(&text, |_| None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("head_poll_ms"), "{error}");
    }
}
