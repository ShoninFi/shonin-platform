//! What the executor says about how a strategy is running.
//!
//! A health report is operational: restarts, faults, refused submissions,
//! venue errors, what the tank has left. It is evidence of how a strategy is
//! running, never a record of what it did — that is on the chain, where
//! nobody can backfill it. Nothing a depositor needs in order to leave
//! depends on it.

use alloy::primitives::Address;
use anyhow::{Context, Result};
use chain::bindings::vault::IVault;
use chain::{Client, Deployments, KeyClient};
use shared::HealthPost;
use shared::model::health::{Entry, Health, Level, Signal};
use time::OffsetDateTime;

use crate::host::Counters;
use crate::scale::{ASSET_DECIMALS, to_decimal};

/// Enough for a strategy to send its transactions for a good while; the tank
/// repays whoever funded the key. A development chain's coin is free; a
/// testnet's comes from a faucet, so a key gets a hundredth of it there.
const GAS_STAKE_DEVELOPMENT: u128 = 1_000_000_000_000_000_000;
const GAS_STAKE_PUBLIC: u128 = 10_000_000_000_000_000;

/// Give a freshly made key something to pay for its own transactions with.
pub async fn fund_gas(chain: &Client, deployments: &Deployments, key: Address) -> Result<()> {
    let stake = if chain.is_development() {
        GAS_STAKE_DEVELOPMENT
    } else {
        GAS_STAKE_PUBLIC
    };
    chain
        .transfer(
            deployments.roles.executor,
            key,
            alloy::primitives::U256::from(stake),
        )
        .await
        .context("staking the key with gas")?;
    Ok(())
}

pub struct Reporter {
    chain: Client,
    vault_chain: shared::Chain,
    key: KeyClient,
    vault: Address,
    api: String,
}

impl Reporter {
    pub fn new(
        chain: Client,
        vault_chain: shared::Chain,
        key: KeyClient,
        vault: Address,
        api: String,
    ) -> Self {
        Self {
            chain,
            vault_chain,
            key,
            vault,
            api,
        }
    }

    /// Send what this instance has seen. A report that cannot be delivered is
    /// worth a line in the log and nothing more: the vault carries on, and a
    /// depositor's exit never waited on it.
    pub async fn post(&self, counters: Counters) {
        match self.build(counters).await {
            Ok(report) => {
                let posted = reqwest::Client::new()
                    .post(format!("{}/api/{}/health", self.api, self.vault_chain))
                    .json(&report)
                    .send()
                    .await;
                if let Err(error) = posted {
                    tracing::debug!(%error, "the report was not delivered");
                }
            }
            Err(error) => tracing::debug!(%error, "could not make a report"),
        }
    }

    async fn build(&self, counters: Counters) -> Result<HealthPost> {
        let tank = self.chain.read(self.vault, IVault::tankCall {}).await?;
        let balance = to_decimal(tank.balance, ASSET_DECIMALS)?;
        // How many days the tank lasts at what the allowances cost a day.
        let allowances = crate::usage::shared_allowances(
            &self
                .chain
                .read(self.vault, IVault::allowancesCall {})
                .await?,
        );
        let daily = shared::Prices::published()
            .daily_cap(&allowances)
            .max(0.000_001);
        let held: f64 = format!("{balance}").parse().unwrap_or(0.0);
        let runway = held / daily;

        let rejection_rate = if counters.submissions == 0 {
            0.0
        } else {
            counters.rejections as f64 / counters.submissions as f64 * 100.0
        };

        let now = OffsetDateTime::now_utc();
        let at = time::PrimitiveDateTime::new(now.date(), now.time());

        let report = Health {
            signals: vec![
                Signal {
                    name: "Restarts".to_string(),
                    value: counters.restarts as f64,
                    unit: "an hour".to_string(),
                    warn_at: 3.0,
                    error_at: 10.0,
                    lower_is_worse: false,
                    window: "the last hour".to_string(),
                    note: None,
                },
                Signal {
                    name: "Rejected submissions".to_string(),
                    value: rejection_rate,
                    unit: "% of submissions".to_string(),
                    warn_at: 5.0,
                    error_at: 25.0,
                    lower_is_worse: false,
                    window: "since it started".to_string(),
                    note: None,
                },
                Signal {
                    name: "Venue errors".to_string(),
                    value: counters.venue_errors as f64,
                    unit: "an hour".to_string(),
                    warn_at: 1.0,
                    error_at: 5.0,
                    lower_is_worse: false,
                    window: "the last hour".to_string(),
                    note: None,
                },
                Signal {
                    name: "Tank".to_string(),
                    value: runway,
                    unit: "days".to_string(),
                    warn_at: 7.0,
                    error_at: 0.0,
                    lower_is_worse: true,
                    window: "at today's charges".to_string(),
                    note: None,
                },
            ],
            entries: vec![Entry {
                at,
                level: if counters.faults > 0 {
                    Level::Warning
                } else {
                    Level::Ok
                },
                message: format!(
                    "{} submissions, {} refused, {} faults",
                    counters.submissions, counters.rejections, counters.faults
                ),
            }],
            updated: at,
            signed_by: format!("{:?}", self.key.address),
        };

        // Signed with the key the registry blessed for this vault, so a
        // reader can tell a report from this instance apart from anybody
        // else's claim about it.
        let vault = format!("{:?}", self.vault);
        let digest = HealthPost::digest(&vault, &report);
        let signature = self
            .key
            .sign(alloy::primitives::B256::from(digest))
            .context("signing the report")?;

        Ok(HealthPost {
            vault,
            report,
            signature: alloy::hex::encode_prefixed(signature),
        })
    }
}
