//! Bounded operational settings shared by chain readers.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::{ChainKey, ConfigError};

macro_rules! settings {
    ($($name:ident),+ $(,)?) => {
        /// One chain's reader settings; all durations are whole milliseconds.
        #[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct RpcPolicy { $(pub $name: u64,)+ }

        /// Only the values explicitly overridden for one chain.
        #[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct RpcOverrides { $($name: Option<u64>,)+ }

        impl RpcOverrides {
            pub fn apply(&self, mut policy: RpcPolicy) -> RpcPolicy {
                $(if let Some(value) = self.$name { policy.$name = value; })+
                policy
            }
        }
    };
}

settings!(
    head_poll_ms,
    event_scan_ms,
    due_lookahead_ms,
    read_budget_ms,
    quiet_probe_budget_ms,
    quiet_probe_retry_ms,
    keeper_rate_limit_initial_ms,
    keeper_rate_limit_max_ms,
    keeper_rate_limit_backoff_factor,
    indexer_poll_ms,
    indexer_idle_max_ms,
    indexer_stale_ms,
    indexer_backoff_factor,
    solana_rescan_ms,
    key_poll_ms,
    key_look_ms,
    history_page_reads,
    ancestry_reads,
    ancestry_cache_headers,
    log_batch_addresses,
    log_batch_blocks,
    log_batch_logs,
    solana_history_transactions,
    view_batch_reads,
    view_batch_input_bytes,
    view_result_bytes,
    view_inner_gas,
    view_outer_gas,
);

/// Global settings and partial per-chain overrides from the same config file.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RpcPolicies {
    pub defaults: RpcPolicy,
    #[serde(default)]
    pub chains: BTreeMap<ChainKey, RpcOverrides>,
}

impl Default for RpcPolicy {
    fn default() -> Self {
        // Tests and direct clients use the same checked-in defaults as services.
        // There is no second set of cadence constants in the reader code.
        #[derive(Deserialize)]
        struct DefaultsFile {
            rpc: RpcPolicies,
        }
        toml::from_str::<DefaultsFile>(include_str!("../../../config/chains.toml"))
            .expect("the checked-in RPC settings are valid")
            .rpc
            .defaults
    }
}

impl RpcPolicies {
    pub fn for_chain(&self, key: &ChainKey) -> RpcPolicy {
        self.chains.get(key).map_or_else(
            || self.defaults.clone(),
            |patch| patch.apply(self.defaults.clone()),
        )
    }
}

impl RpcPolicy {
    /// Validate resource and cadence relationships. The keeper must additionally
    /// validate each vault's timers before using a policy to schedule its work.
    pub fn validate(&self, name: &str) -> Result<(), ConfigError> {
        let bounded = |field: &str, value: u64, min: u64, max: u64| {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(ConfigError::new(format!(
                    "RPC settings for {name}: {field}={value}, expected {min}..={max}"
                )))
            }
        };
        for (field, value) in [
            ("head_poll_ms", self.head_poll_ms),
            ("event_scan_ms", self.event_scan_ms),
            ("due_lookahead_ms", self.due_lookahead_ms),
            ("read_budget_ms", self.read_budget_ms),
            ("indexer_poll_ms", self.indexer_poll_ms),
            ("indexer_idle_max_ms", self.indexer_idle_max_ms),
            ("indexer_stale_ms", self.indexer_stale_ms),
            ("key_poll_ms", self.key_poll_ms),
            ("key_look_ms", self.key_look_ms),
        ] {
            bounded(field, value, 1, 86_400_000)?;
        }
        // Key relay and native-gas stocking retain their established maximum
        // wait. Longer cadences need a separate key-expiry and funding proof.
        bounded("key_poll_ms", self.key_poll_ms, 1, 2_000)?;
        // Event/due observation cannot stretch the established active round.
        bounded("head_poll_ms", self.head_poll_ms, 1, 2_000)?;
        bounded("event_scan_ms", self.event_scan_ms, 1, 2_000)?;
        bounded(
            "quiet_probe_budget_ms",
            self.quiet_probe_budget_ms,
            1,
            30_000,
        )?;
        bounded(
            "quiet_probe_retry_ms",
            self.quiet_probe_retry_ms,
            1,
            86_400_000,
        )?;
        bounded("key_look_ms", self.key_look_ms, 1, 15_000)?;
        bounded(
            "keeper_rate_limit_initial_ms",
            self.keeper_rate_limit_initial_ms,
            1,
            5_000,
        )?;
        bounded(
            "keeper_rate_limit_max_ms",
            self.keeper_rate_limit_max_ms,
            1,
            120_000,
        )?;
        bounded(
            "keeper_rate_limit_backoff_factor",
            self.keeper_rate_limit_backoff_factor,
            1,
            2,
        )?;
        if self.keeper_rate_limit_max_ms < self.keeper_rate_limit_initial_ms {
            return Err(ConfigError::new(format!(
                "RPC settings for {name}: keeper_rate_limit_max_ms={} is below keeper_rate_limit_initial_ms={}",
                self.keeper_rate_limit_max_ms, self.keeper_rate_limit_initial_ms
            )));
        }
        bounded("solana_rescan_ms", self.solana_rescan_ms, 0, 86_400_000)?;
        bounded("indexer_backoff_factor", self.indexer_backoff_factor, 1, 16)?;
        bounded("history_page_reads", self.history_page_reads, 1, 4_096)?;
        bounded("ancestry_reads", self.ancestry_reads, 1, 4_096)?;
        bounded(
            "ancestry_cache_headers",
            self.ancestry_cache_headers,
            1,
            65_536,
        )?;
        bounded("log_batch_addresses", self.log_batch_addresses, 1, 1_024)?;
        bounded("log_batch_blocks", self.log_batch_blocks, 1, 10_000)?;
        let needed = self.log_batch_blocks + self.ancestry_reads * 2 + 2;
        if self.ancestry_cache_headers < needed {
            return Err(ConfigError::new(format!(
                "RPC settings for {name}: ancestry_cache_headers={} needs at least {needed} for log_batch_blocks={} and ancestry_reads={}",
                self.ancestry_cache_headers, self.log_batch_blocks, self.ancestry_reads
            )));
        }
        bounded("log_batch_logs", self.log_batch_logs, 1, 100_000)?;
        bounded(
            "solana_history_transactions",
            self.solana_history_transactions,
            1,
            4_096,
        )?;
        bounded("view_batch_reads", self.view_batch_reads, 1, 64)?;
        bounded(
            "view_batch_input_bytes",
            self.view_batch_input_bytes,
            4,
            1_048_576,
        )?;
        bounded("view_result_bytes", self.view_result_bytes, 32, 16_384)?;
        bounded("view_inner_gas", self.view_inner_gas, 1, 1_000_000)?;
        bounded("view_outer_gas", self.view_outer_gas, 21_000, 100_000_000)?;
        if self.indexer_idle_max_ms < self.indexer_poll_ms {
            return Err(ConfigError::new(format!(
                "RPC settings for {name}: indexer_idle_max_ms={} is below indexer_poll_ms={}",
                self.indexer_idle_max_ms, self.indexer_poll_ms
            )));
        }
        if self.indexer_stale_ms < self.indexer_idle_max_ms.saturating_mul(2) {
            return Err(ConfigError::new(format!(
                "RPC settings for {name}: indexer_stale_ms={} must cover twice indexer_idle_max_ms={}",
                self.indexer_stale_ms, self.indexer_idle_max_ms
            )));
        }
        let margin = self.head_poll_ms + self.event_scan_ms + self.read_budget_ms;
        if self.due_lookahead_ms < margin {
            return Err(ConfigError::new(format!(
                "RPC settings for {name}: due_lookahead_ms={} is below head_poll_ms + event_scan_ms + read_budget_ms ({margin})",
                self.due_lookahead_ms
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_defaults_and_partial_overrides_preserve_other_values() {
        let original = RpcPolicy::default();
        original.validate("defaults").unwrap();
        let patch: RpcOverrides = toml::from_str("indexer_poll_ms = 1000").unwrap();
        let modified = patch.apply(original.clone());
        assert_eq!(modified.indexer_poll_ms, 1000);
        assert_eq!(modified.head_poll_ms, original.head_poll_ms);
        assert_eq!(modified.solana_rescan_ms, original.solana_rescan_ms);
    }

    #[test]
    fn idle_health_margin_accepts_the_boundary_and_refuses_one_millisecond_less() {
        for active in [1, 2_000, 60_000] {
            for idle in [active, active * 2, active * 4] {
                let mut policy = RpcPolicy {
                    indexer_poll_ms: active,
                    indexer_idle_max_ms: idle,
                    indexer_stale_ms: idle * 2,
                    ..Default::default()
                };
                policy.validate("a record reader").unwrap();
                policy.indexer_stale_ms -= 1;
                assert!(
                    policy
                        .validate("a record reader")
                        .unwrap_err()
                        .to_string()
                        .contains("indexer_stale_ms")
                );
            }
        }
    }

    #[test]
    fn lookahead_accepts_its_exact_margin_and_rejects_one_millisecond_less() {
        for head in [1, 1_000, 2_000] {
            for events in [1, 1_000, 2_000] {
                for budget in [1, 1_000, 30_000] {
                    let mut policy = RpcPolicy {
                        head_poll_ms: head,
                        event_scan_ms: events,
                        read_budget_ms: budget,
                        due_lookahead_ms: head + events + budget,
                        ..RpcPolicy::default()
                    };
                    policy.validate("a chain").unwrap();
                    policy.due_lookahead_ms -= 1;
                    assert!(
                        policy
                            .validate("a chain")
                            .unwrap_err()
                            .to_string()
                            .contains("due_lookahead_ms")
                    );
                }
            }
        }
    }

    #[test]
    fn key_cadences_never_extend_the_existing_relay_and_stocking_wait() {
        for poll in [1, 100, 2_000] {
            for look in [1, 1_000, 15_000] {
                RpcPolicy {
                    key_poll_ms: poll,
                    key_look_ms: look,
                    ..Default::default()
                }
                .validate("keys")
                .unwrap();
            }
        }
        for policy in [
            RpcPolicy {
                key_poll_ms: 2_001,
                ..Default::default()
            },
            RpcPolicy {
                key_look_ms: 15_001,
                ..Default::default()
            },
        ] {
            assert!(
                policy
                    .validate("keys")
                    .unwrap_err()
                    .to_string()
                    .contains("key_")
            );
        }
    }

    #[test]
    fn zero_cadences_and_unbounded_batches_name_the_rejected_setting() {
        let policy = RpcPolicy {
            head_poll_ms: 0,
            ..RpcPolicy::default()
        };
        assert!(
            policy
                .validate("a chain")
                .unwrap_err()
                .to_string()
                .contains("head_poll_ms=0")
        );
        let policy = RpcPolicy {
            view_batch_reads: 65,
            ..RpcPolicy::default()
        };
        assert!(
            policy
                .validate("a chain")
                .unwrap_err()
                .to_string()
                .contains("view_batch_reads=65")
        );
        assert!(toml::from_str::<RpcOverrides>("unknown_cadence_ms = 1").is_err());
    }
}
