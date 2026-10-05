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

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{B256, U256};
use alloy::providers::Provider;

use chain::bindings::attestation::IAttestationRegistry;

use crate::ceilings::Ceilings;
use crate::host::Counters;
use crate::scale::{ASSET_DECIMALS, to_decimal};

/// Enough for a strategy to send its transactions for a good while. The stake
/// is Shonin's: what the agent's own transactions spend is billed to the tank
/// through the meter, and what the platform sends with the key (its blessing,
/// the venue's approval) is not billed to anyone. A development chain's coin
/// is free; a testnet's comes from a faucet, so a key gets a hundredth of it
/// there.
const GAS_STAKE_DEVELOPMENT: u128 = 1_000_000_000_000_000_000;
const GAS_STAKE_PUBLIC: u128 = 10_000_000_000_000_000;

/// The stake a key is given on this chain, before fees are counted.
pub fn gas_stake(chain: &Client) -> u128 {
    if chain.is_development() {
        GAS_STAKE_DEVELOPMENT
    } else {
        GAS_STAKE_PUBLIC
    }
}

/// The stake at today's fee: the chain's stake, or three of the key's
/// worst-case sends if that is more. The lines below sit in order under it:
/// the top-up at two sends at least, the warning at half the stake (a send
/// and a half at least), the error at one send. So a key is topped up before
/// it reads as low, and reads as low before it can't send.
fn stake_at(base: u128, worst_send: u128) -> u128 {
    base.max(worst_send.saturating_mul(3))
}

/// Below what a key is topped up: the stake less one worst-case send, so the
/// key is brought back up after every few sends rather than left to fall to
/// the warning at half a stake. Never below two worst-case sends, so a key is
/// topped up while it can still pay for its next one and before it reads as
/// low, however the stake compares with the fee. A fee that couldn't be read
/// tops up at half a stake.
fn top_up_below(stake: u128, worst_send: Option<u128>) -> u128 {
    match worst_send {
        Some(send) => stake.saturating_sub(send).max(send.saturating_mul(2)),
        None => stake / 2,
    }
}

/// What one of the key's larger sends (its blessing, an approval) can cost
/// at most at today's fees: the gas it is given, at the most per gas it may
/// be charged. The node won't take a send from a key that holds less, whatever
/// it would end up paying. The most per gas is what an EIP-1559 send is filled
/// with; a chain that can't say falls back to its gas price, as a send does.
pub async fn worst_send(chain: &Client) -> Result<u128> {
    let per_gas = match chain.provider().estimate_eip1559_fees().await {
        Ok(fees) => fees.max_fee_per_gas,
        Err(_) => chain
            .provider()
            .get_gas_price()
            .await
            .context("reading the gas price")?,
    };
    Ok(per_gas.saturating_mul(ONE_SEND_GAS))
}

/// The stakes sent to each key and not yet seen to land, by their hash and
/// when they were sent. Kept by the sweep, so a vault taken on again finds
/// the stake an earlier attempt sent.
pub type Stakes = Arc<Mutex<HashMap<Address, (B256, Instant)>>>;

/// How long a stake that was sent is waited for before it is taken for lost:
/// as long as the chain client keeps looking for a send's receipt.
const STAKE_LANDING: Duration = Duration::from_secs(600);

/// Keep a key stocked with gas: when it holds less than a stake less one
/// worst-case send, send it what brings it back up to one.
///
/// A stake that was sent and has not yet been seen to land (its receipt
/// unread, the chain slow to mine it) is waited for rather than sent again,
/// and is no reason to fail the caller: it is on its way. A key's balance is
/// read in the pending state as well, so a stake still in the pool counts
/// even when this instance never learnt its hash.
///
/// Says whether the key holds its gas now: whether its balance, as of the
/// latest block, is at the top-up line. A stake still on its way is not gas
/// the key can spend yet.
pub async fn fund_gas(
    chain: &Client,
    deployments: &Deployments,
    stakes: &Stakes,
    key: Address,
) -> Result<bool> {
    let remembered = stakes
        .lock()
        .ok()
        .and_then(|stakes| stakes.get(&key).copied());
    let mut landing = false;
    if let Some((hash, sent)) = remembered
        && sent.elapsed() < STAKE_LANDING
    {
        match chain.provider().get_transaction_receipt(hash).await {
            // Landed or failed: the balance now says what the key holds.
            Ok(Some(_)) => {}
            // Still on its way, or no telling yet.
            Ok(None) | Err(_) => landing = true,
        }
    }
    if !landing && let Ok(mut stakes) = stakes.lock() {
        stakes.remove(&key);
    }

    let send = worst_send(chain).await.ok();
    let stake = stake_at(gas_stake(chain), send.unwrap_or(0));
    let below = U256::from(top_up_below(stake, send));
    let stake = U256::from(stake);
    let latest = chain
        .balance(key)
        .await
        .context("reading what gas the key holds")?;
    if latest >= below {
        return Ok(true);
    }
    if landing {
        return Ok(false);
    }
    let pending = chain
        .provider()
        .get_balance(key)
        .pending()
        .await
        .unwrap_or(latest);
    let holds = latest.max(pending);
    if holds >= below {
        return Ok(false);
    }
    let noting = stakes.clone();
    chain
        .transfer_noting(
            deployments.roles.executor,
            key,
            stake - holds,
            move |hash| {
                if let Ok(mut stakes) = noting.lock() {
                    stakes.insert(key, (hash, Instant::now()));
                }
            },
        )
        .await
        .context("staking the key with gas")?;
    if let Ok(mut stakes) = stakes.lock() {
        stakes.remove(&key);
    }
    Ok(true)
}

/// How often a served key's gas is looked at.
const TOP_UP_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(60)
};

/// Keep a served vault's key stocked with gas for as long as the vault is
/// served. Shonin pays for the key's gas; the agent's share is billed to the
/// tank as it is spent. A key left to run dry could no longer trade, nor
/// renew the blessing that its reports and signed totals depend on.
pub async fn keep_stocked(
    chain: Client,
    deployments: Deployments,
    stakes: Stakes,
    key: Address,
    vault: Address,
) {
    let mut unstocked = crate::Throttle::default();
    loop {
        tokio::time::sleep(TOP_UP_EVERY).await;
        if let Err(error) = fund_gas(&chain, &deployments, &stakes, key).await
            && unstocked.due(Instant::now())
        {
            tracing::warn!(%vault, %key, error = format!("{error:#}"), "could not top the key up with gas");
        }
    }
}

/// Roughly what one of the key's larger sends uses, such as its blessing: a
/// key holding less than this at the most per gas it may be charged can't be
/// relied on to send anything.
const ONE_SEND_GAS: u128 = 300_000;

/// The key's gas, in the chain's coin: a warning below half a stake (the
/// top-up is late), an error below one worst-case send at today's fees. The
/// stake is at least three such sends and a key is topped up below two, so a
/// key just topped up reads as fine and one at the top-up line is not yet
/// low.
fn key_gas(holds: U256, stake: u128, worst_send: u128) -> Signal {
    let coin = |wei: f64| wei / 1e18;
    Signal {
        name: "Key gas".to_string(),
        value: coin(u128::try_from(holds).unwrap_or(u128::MAX) as f64),
        unit: "coin".to_string(),
        // A signal reads as past a threshold at it, and the key is only short
        // below it: the threshold sits just under the line.
        warn_at: coin((stake / 2) as f64).next_down(),
        error_at: coin(worst_send as f64).next_down(),
        lower_is_worse: true,
        window: "now".to_string(),
        note: Some(
            "Shonin keeps the key stocked; the agent's gas is billed to the tank as it is spent"
                .to_string(),
        ),
    }
}

const HOUR: Duration = Duration::from_secs(3_600);

/// The counts a report gives "an hour".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Hourly {
    restarts: u64,
    venue_errors: u64,
}

/// What changed in the last hour: the counters now, less the newest reading
/// at least an hour old, or the oldest there is. Readings older than that one
/// are forgotten; it is kept, so that after a gap in the readings the count
/// still starts from the last one before it rather than from nothing.
fn last_hour(
    window: &mut VecDeque<(Instant, Counters)>,
    now: Instant,
    counters: Counters,
) -> Hourly {
    while window
        .get(1)
        .is_some_and(|(at, _)| now.duration_since(*at) >= HOUR)
    {
        window.pop_front();
    }
    let before = window
        .front()
        .map(|(_, counters)| *counters)
        .unwrap_or_default();
    window.push_back((now, counters));
    Hourly {
        restarts: counters.restarts.saturating_sub(before.restarts),
        venue_errors: counters.venue_errors.saturating_sub(before.venue_errors),
    }
}

pub struct Reporter {
    chain: Client,
    vault_chain: shared::Chain,
    attestation: Address,
    key: KeyClient,
    vault: Address,
    api: String,
    /// Bounded, so an API that stops answering never holds up the next
    /// report.
    http: reqwest::Client,
    /// The counters as they stood at each report in the last hour, oldest
    /// first, so a count "an hour" is what happened in the last hour.
    window: VecDeque<(Instant, Counters)>,
    /// Whether the tank is dry, as the meter last found it.
    dry: Arc<AtomicBool>,
    /// The meter's latest signed totals, so the tank's runway counts what is
    /// owed but not yet collected.
    totals: crate::usage::Totals,
    /// Whether reports are being held for want of a blessing, so that is said
    /// once rather than at every report.
    holding: bool,
    /// When a report last failed for want of the chain, so a chain that is
    /// down is said about once a minute rather than at every report.
    unread: crate::Throttle,
    /// When a report last failed to be delivered or was refused, so an API
    /// that is down or turning reports away is said about once a minute.
    undelivered: crate::Throttle,
    /// The most of each allowance the agent is given here: the runway is
    /// counted at what is billed, and an allowance held below what the
    /// author asked for is said.
    ceilings: Ceilings,
}

impl Reporter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain: Client,
        vault_chain: shared::Chain,
        attestation: Address,
        key: KeyClient,
        vault: Address,
        api: String,
        dry: Arc<AtomicBool>,
        totals: crate::usage::Totals,
        ceilings: Ceilings,
    ) -> Self {
        Self {
            chain,
            vault_chain,
            attestation,
            key,
            vault,
            api,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            // Nothing had happened when the vault was taken on.
            window: VecDeque::from([(Instant::now(), Counters::default())]),
            dry,
            totals,
            holding: false,
            unread: Default::default(),
            undelivered: Default::default(),
            ceilings,
        }
    }

    /// Send what this instance has seen. A report that cannot be delivered is
    /// worth a line in the log and nothing more: the vault carries on, and a
    /// depositor's exit never waited on it.
    pub async fn post(&mut self, counters: Counters) {
        let hourly = last_hour(&mut self.window, Instant::now(), counters);
        // Nobody would believe a report from a key the vault hasn't blessed,
        // so there is no sending one: before the blessing lands, or once it
        // has lapsed with the vault.
        match self.blessed().await {
            Ok(true) => self.holding = false,
            Ok(false) => {
                if !self.holding {
                    tracing::info!(vault = %self.vault, "not blessed yet; holding the report");
                }
                self.holding = true;
                return;
            }
            Err(error) => {
                if self.unread.due(Instant::now()) {
                    tracing::warn!(vault = %self.vault, error = format!("{error:#}"), "could not tell whether the key is blessed; no report this time");
                }
                return;
            }
        }
        match self.build(counters, hourly).await {
            Ok(report) => {
                let posted = self
                    .http
                    .post(format!("{}/api/{}/health", self.api, self.vault_chain))
                    .json(&report)
                    .send()
                    .await;
                match posted {
                    Ok(answer) if answer.status().is_success() => {}
                    Ok(answer) => {
                        if self.undelivered.due(Instant::now()) {
                            let status = answer.status();
                            let why = answer.text().await.unwrap_or_default();
                            tracing::warn!(vault = %self.vault, %status, why, "the API refused the report");
                        }
                    }
                    Err(error) => {
                        if self.undelivered.due(Instant::now()) {
                            tracing::warn!(vault = %self.vault, %error, "the report was not delivered");
                        }
                    }
                }
            }
            Err(error) => {
                if self.unread.due(Instant::now()) {
                    tracing::warn!(vault = %self.vault, error = format!("{error:#}"), "could not make a report")
                }
            }
        }
    }

    async fn blessed(&self) -> Result<bool> {
        let bundle = self
            .chain
            .read(self.vault, IVault::bundleHashCall {})
            .await?;
        self.chain
            .read(
                self.attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer: self.key.address,
                    vaultChain: alloy::primitives::U256::from(self.chain.chain_id),
                    vault: self.vault,
                    strategyHash: bundle,
                },
            )
            .await
    }

    async fn build(&self, counters: Counters, hourly: Hourly) -> Result<HealthPost> {
        let tank = self.chain.read(self.vault, IVault::tankCall {}).await?;
        let asked = crate::usage::shared_allowances(
            &self
                .chain
                .read(self.vault, IVault::allowancesCall {})
                .await?,
        );
        let held_below = self.ceilings.held_below(&asked);
        let daily = shared::Prices::published().daily_cap(&self.ceilings.hold(&asked));
        let signed = self
            .totals
            .lock()
            .ok()
            .and_then(|totals| totals.get(&(self.chain.chain_id, self.vault)).cloned())
            .and_then(|signed| signed.total.parse::<U256>().ok())
            .unwrap_or_default();
        let unpaid = signed.saturating_sub(tank.usageSettled);
        let dry = self.dry.load(Ordering::SeqCst);
        let runway = runway(tank.balance, unpaid, daily, dry)?;
        let rejection_rate = counters.rejection_rate();
        let holds = self.chain.balance(self.key.address).await?;
        let send = worst_send(&self.chain).await?;

        let now = OffsetDateTime::now_utc();
        let at = time::PrimitiveDateTime::new(now.date(), now.time());

        let report = Health {
            signals: vec![
                Signal {
                    name: "Restarts".to_string(),
                    value: hourly.restarts as f64,
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
                    value: hourly.venue_errors as f64,
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
                key_gas(holds, stake_at(gas_stake(&self.chain), send), send),
            ],
            entries: entries(at, counters, dry, &held_below),
            updated: at,
            signed_by: format!("{:?}", self.key.address),
        };

        // Signed with the key the registry blessed for this vault, so a
        // reader can tell a report from this instance apart from anybody
        // else's claim about it.
        let vault = format!("{:?}", self.vault);
        let digest = HealthPost::digest(self.vault_chain, &vault, &report);
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

/// How many days the tank lasts at what the allowances cost a day, counting
/// only what is left once the usage owed but not yet collected is paid. A dry
/// tank lasts no time at all: the strategy is stopped.
fn runway(balance: U256, unpaid: U256, daily: f64, dry: bool) -> Result<f64> {
    if dry {
        return Ok(0.0);
    }
    let left = to_decimal(balance.saturating_sub(unpaid), ASSET_DECIMALS)?;
    let left: f64 = format!("{left}").parse().unwrap_or(0.0);
    Ok(left / daily.max(0.000_001))
}

/// What the report says happened: the counts, and while the tank is dry, that
/// the strategy is stopped until it is topped up. A strategy stopped for its
/// tank leaves its positions as they are, and that is an error worth saying.
/// An allowance held below what the author asked for (`held_below`) is a
/// warning: the agent runs, at less than its author meant it to.
fn entries(
    at: time::PrimitiveDateTime,
    counters: Counters,
    dry: bool,
    held_below: &[String],
) -> Vec<Entry> {
    let mut entries = vec![Entry {
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
    }];
    if counters.not_the_bundle > 0 {
        entries.push(Entry {
            at,
            level: Level::Error,
            message: format!(
                "The component in the executor's store is not the vault's bundle: it was refused {} times and never run",
                counters.not_the_bundle
            ),
        });
    }
    if !held_below.is_empty() {
        entries.push(Entry {
            at,
            level: Level::Warning,
            message: format!(
                "The allowances ask for more than one agent is given on this executor, and the agent runs, and is billed, at its ceilings: {}",
                held_below.join("; ")
            ),
        });
    }
    if dry {
        entries.push(Entry {
            at,
            level: Level::Error,
            message: "The tank is dry: the strategy is stopped, its positions as they are, until the tank is topped up".to_string(),
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(restarts: u64, venue_errors: u64) -> Counters {
        Counters {
            restarts,
            venue_errors,
            ..Counters::default()
        }
    }

    #[tokio::test]
    async fn a_stake_still_in_the_pool_is_not_sent_again() {
        // Every stake sits in the pool: the latest block shows nothing, the
        // pending state shows it, and its receipt is never read.
        let state = crate::stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().in_the_pool = true;
        let client = crate::stand_in::client(&crate::stand_in::start(state.clone()).await).await;
        let deployments = crate::stand_in::deployments();
        let key = Address::repeat_byte(0x55);
        let stakes = Stakes::default();
        let sent = || state.lock().unwrap().sent.len();

        // The first take-on: the stake goes out and the wait for it runs
        // out. The clock is paused once it is out, so that is at once.
        let first = fund_gas(&client, &deployments, &stakes, key);
        let staked = async {
            while sent() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::pause();
        };
        let (first, ()) = tokio::join!(first, staked);
        tokio::time::resume();
        assert!(first.is_err(), "the stake was never seen to land");
        assert_eq!(sent(), 1);

        // Tried again: the stake is on its way, so it is neither sent again
        // nor a reason to fail.
        fund_gas(&client, &deployments, &stakes, key).await.unwrap();
        assert_eq!(sent(), 1, "a second stake went out after the first");

        // An instance that never learnt the stake's hash sees it pending.
        fund_gas(&client, &deployments, &Stakes::default(), key)
            .await
            .unwrap();
        assert_eq!(sent(), 1, "a stake in the pool was not counted");
    }

    #[test]
    fn a_key_low_on_gas_is_a_warning_and_one_that_cannot_send_is_an_error() {
        let send = 1_000_000_000 * ONE_SEND_GAS;
        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        let level = |holds: u128| key_gas(U256::from(holds), stake, send).level();
        assert_eq!(level(stake), Level::Ok);
        assert_eq!(level(stake / 3), Level::Warning);
        assert_eq!(level(send / 2), Level::Error);
    }

    #[test]
    fn a_key_is_topped_up_once_it_is_a_send_short_of_its_stake_before_it_reads_as_low() {
        let send = 1_000_000_000 * ONE_SEND_GAS;
        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        let below = top_up_below(stake, Some(send));
        assert_eq!(below, stake - send);
        // A key at the top-up line is still well clear of the warning.
        assert_eq!(key_gas(U256::from(below), stake, send).level(), Level::Ok);
        // A fee that couldn't be read tops up at half a stake.
        assert_eq!(top_up_below(stake, None), stake / 2);
    }

    #[test]
    fn a_stake_covers_three_worst_case_sends_and_is_topped_up_before_the_key_reads_as_low() {
        // A send that costs three times the public stake.
        let send = 3 * GAS_STAKE_PUBLIC;
        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        assert_eq!(stake, 3 * send);
        let below = top_up_below(stake, Some(send));
        assert_eq!(below, 2 * send);
        let level = |holds: u128| key_gas(U256::from(holds), stake, send).level();
        // A key just topped up reads as fine, and so does one at the top-up
        // line: it is topped up before it reads as low.
        assert_eq!(level(stake), Level::Ok);
        assert_eq!(level(below), Level::Ok);
        // Below half the stake it reads as low, and below one send it can't
        // send.
        assert_eq!(level(stake / 2 - send / 10), Level::Warning);
        assert_eq!(level(send - send / 10), Level::Error);
        // Whatever the stake is told, the line never falls below two sends.
        assert_eq!(top_up_below(send, Some(send)), 2 * send);
        // A cheap chain keeps its stake.
        assert_eq!(stake_at(GAS_STAKE_PUBLIC, 1), GAS_STAKE_PUBLIC);
    }

    const GWEI: u128 = 1_000_000_000;

    /// On a public chain at 25 gwei a send may be charged at twice the base
    /// fee, so one of the key's larger sends can cost about 0.015 of the coin.
    /// A key holding 0.006 can't pay for its blessing, and is topped up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_that_cannot_pay_for_one_send_is_topped_up() {
        let holds = 6 * GAS_STAKE_PUBLIC / 10;
        let (node, client) = crate::stand_in::public::start(25 * GWEI, holds).await;
        let send = worst_send(&client).await.unwrap();
        assert!(
            holds < send,
            "the key can't pay for one of its larger sends"
        );

        fund_gas(
            &client,
            &crate::stand_in::deployments(),
            &Default::default(),
            Address::repeat_byte(0x55),
        )
        .await
        .unwrap();

        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        assert_eq!(
            node.lock().unwrap().sent,
            [U256::from(stake - holds)],
            "a key holding less than one send was not brought up to its stake"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_less_than_a_send_short_of_its_stake_is_left_as_it_is() {
        let (node, client) = crate::stand_in::public::start(25 * GWEI, 0).await;
        let send = worst_send(&client).await.unwrap();
        let stake = stake_at(GAS_STAKE_PUBLIC, send);
        node.lock().unwrap().key_holds = stake - send / 2;

        fund_gas(
            &client,
            &crate::stand_in::deployments(),
            &Default::default(),
            Address::repeat_byte(0x55),
        )
        .await
        .unwrap();

        assert!(
            node.lock().unwrap().sent.is_empty(),
            "a well-stocked key was topped up"
        );
    }

    /// A report posted to a stand-in API, read back.
    async fn one_report(client: Client, ceilings: Ceilings) -> HealthPost {
        use axum::{Json, Router, routing::post};
        let posted: Arc<Mutex<Option<HealthPost>>> = Default::default();
        let keep = posted.clone();
        let app = Router::new().route(
            "/api/{chain}/health",
            post(move |Json(post): Json<HealthPost>| {
                let keep = keep.clone();
                async move {
                    *keep.lock().unwrap() = Some(post);
                    axum::http::StatusCode::ACCEPTED
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut reporter = Reporter::new(
            client.clone(),
            shared::Chain::Base,
            Address::ZERO,
            crate::stand_in::key(&client),
            Address::repeat_byte(0x11),
            api,
            Default::default(),
            Default::default(),
            ceilings,
        );
        reporter.post(Default::default()).await;
        let report = posted.lock().unwrap().take();
        report.expect("the report was posted")
    }

    /// On a public chain at 100 gwei one send costs more than the public
    /// stake. A key holding its whole stake, as it does just after a top-up,
    /// reads as fine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_holding_a_full_stake_does_not_read_as_an_error() {
        let (node, client) = crate::stand_in::public::start(100 * GWEI, 0).await;
        let send = worst_send(&client).await.unwrap();
        assert!(
            send > GAS_STAKE_PUBLIC,
            "one send costs more than the stake"
        );
        node.lock().unwrap().key_holds = stake_at(gas_stake(&client), send);

        let post = one_report(client, Ceilings::default()).await;
        let key_gas = post
            .report
            .signals
            .iter()
            .find(|signal| signal.name == "Key gas")
            .expect("the report has the key's gas");
        assert_eq!(
            key_gas.level(),
            Level::Ok,
            "a key holding a full stake reads as low: holds {}, warning at {}, error at {}",
            key_gas.value,
            key_gas.warn_at,
            key_gas.error_at
        );
    }

    /// The vault asks for the modest shape's 256 MiB on an executor whose
    /// ceiling is 64 MiB: its report warns that the agent is held to the
    /// ceiling, and its runway is counted at what is billed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_allowance_held_to_its_ceiling_is_said_in_the_health_report() {
        let (node, client) = crate::stand_in::public::start(GWEI, 0).await;
        node.lock().unwrap().key_holds = GAS_STAKE_PUBLIC;
        let held = Ceilings {
            memory_mib: 64,
            ..Ceilings::default()
        };

        let post = one_report(client.clone(), Ceilings::default()).await;
        assert!(
            post.report
                .entries
                .iter()
                .all(|entry| !entry.message.contains("ceiling")),
            "{:?}",
            post.report.entries
        );
        let in_full = post
            .report
            .signals
            .iter()
            .find(|s| s.name == "Tank")
            .unwrap()
            .value;

        let post = one_report(client, held).await;
        let said = post
            .report
            .entries
            .iter()
            .find(|entry| entry.message.contains("ceiling"))
            .unwrap_or_else(|| panic!("nothing said of the ceiling: {:?}", post.report.entries));
        assert_eq!(said.level, Level::Warning);
        assert!(
            said.message.contains("memory 256 MiB, held to 64"),
            "{}",
            said.message
        );
        let held_runway = post
            .report
            .signals
            .iter()
            .find(|s| s.name == "Tank")
            .unwrap()
            .value;
        assert!(
            held_runway > in_full,
            "held to less, the tank lasts longer: {held_runway} against {in_full}"
        );
    }

    #[test]
    fn an_hour_counts_only_the_last_hour() {
        let start = Instant::now();
        let mut readings = VecDeque::from([(start, Counters::default())]);
        let first = start + Duration::from_secs(15);
        assert_eq!(last_hour(&mut readings, first, at(4, 2)).restarts, 4);
        let later = start + Duration::from_secs(1_800);
        assert_eq!(last_hour(&mut readings, later, at(5, 2)).restarts, 5);
        // Ninety minutes in, only what happened since the reading an hour ago.
        let much_later = start + Duration::from_secs(5_400);
        let hourly = last_hour(&mut readings, much_later, at(7, 3));
        assert_eq!(
            hourly,
            Hourly {
                restarts: 2,
                venue_errors: 1
            }
        );
    }

    #[test]
    fn an_hour_after_a_gap_counts_from_the_last_reading_before_it() {
        let start = Instant::now();
        let mut readings = VecDeque::from([(start, Counters::default())]);
        last_hour(&mut readings, start + Duration::from_secs(15), at(4, 2));
        // Two hours with no reading: what happened since the last one is all
        // that can be said, not everything since the start.
        let after_the_gap = start + Duration::from_secs(7_200);
        let hourly = last_hour(&mut readings, after_the_gap, at(7, 3));
        assert_eq!(
            hourly,
            Hourly {
                restarts: 3,
                venue_errors: 1
            }
        );
    }

    #[test]
    fn the_tank_lasts_what_is_left_once_the_unpaid_usage_is_paid() {
        // $70 in the tank, $35 owed, at $5 a day: a week.
        let days = runway(
            U256::from(70_000_000u64),
            U256::from(35_000_000u64),
            5.0,
            false,
        )
        .unwrap();
        assert_eq!(days, 7.0);
        // Owing more than it holds: nothing left.
        let days = runway(
            U256::from(1_000_000u64),
            U256::from(2_000_000u64),
            5.0,
            false,
        )
        .unwrap();
        assert_eq!(days, 0.0);
    }

    #[test]
    fn a_dry_tank_reads_as_an_error_with_the_strategy_stopped() {
        // The meter found it dry, whatever the balance reads.
        let days = runway(U256::from(70_000_000u64), U256::ZERO, 5.0, true).unwrap();
        assert_eq!(days, 0.0);
        let tank = Signal {
            name: "Tank".to_string(),
            value: days,
            unit: "days".to_string(),
            warn_at: 7.0,
            error_at: 0.0,
            lower_is_worse: true,
            window: "at today's charges".to_string(),
            note: None,
        };
        assert_eq!(tank.level(), Level::Error);

        let now = OffsetDateTime::now_utc();
        let at = time::PrimitiveDateTime::new(now.date(), now.time());
        let said = entries(at, Counters::default(), true, &[]);
        assert!(
            said.iter()
                .any(|entry| entry.level == Level::Error && entry.message.contains("stopped")),
            "{said:?}"
        );
        assert!(
            entries(at, Counters::default(), false, &[])
                .iter()
                .all(|entry| entry.level != Level::Error)
        );
    }

    #[test]
    fn a_component_that_is_not_the_bundle_reads_as_an_error() {
        let now = OffsetDateTime::now_utc();
        let at = time::PrimitiveDateTime::new(now.date(), now.time());
        let refused = Counters {
            not_the_bundle: 2,
            ..Counters::default()
        };
        let said = entries(at, refused, false, &[]);
        assert!(
            said.iter().any(|entry| entry.level == Level::Error
                && entry.message.contains("not the vault's bundle")),
            "{said:?}"
        );
    }
}
