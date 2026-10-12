//! What a Solana vault's agent costs, signed for the keeper to collect, and
//! how it is running, signed for anyone to read.
//!
//! The meter is the EVM one's: capacity held while the agent runs, billed by
//! the wall clock at the published prices, and gas billed as it is spent and
//! never beyond the gas allowance. Gas here is lamports: the fees and the
//! rent of the agent's own sends, less rent taken back.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use anyhow::Context;
use chain::solana::executor::{Blessed, SignedTotal};
use chain::solana::programs::vault::state::Allowed;
use chain::solana::{Keypair, Platform, Pubkey, Signer, SolanaClient};
use shared::HealthPost;
use shared::model::health::Signal;

use crate::ceilings::Ceilings;
use crate::health::{Holds, Hourly, last_hour, report, runway};
use crate::host::Counters;
use crate::usage::{AGREE_BLOCKS, Meter, SAMPLE, SAMPLES_PER_TICK, SignedThrough};

/// The latest signed total of every Solana vault this executor serves, by
/// cluster name and vault address.
pub type Totals = Arc<Mutex<std::collections::HashMap<(String, String), SignedTotal>>>;

/// A lamport, as the meter counts gas: in the smallest unit of an 18-decimal
/// coin, as it counts wei.
const LAMPORT_AS_WEI: u128 = 1_000_000_000;

/// The vault's allowances as the platform's own type.
pub fn allowances(on_chain: &Allowed) -> shared::Allowances {
    shared::Allowances {
        fuel_per_sec: on_chain.fuel_per_sec,
        memory_mib: on_chain.memory_mib,
        requests_per_min: on_chain.requests_per_min,
        bytes_per_day: on_chain.bytes_per_day,
        streams: on_chain.streams,
        inference_ms_per_min: on_chain.inference_ms_per_min,
        gas_per_day: on_chain.gas_per_day as f64 / 1_000_000.0,
    }
}

/// What the meter shares with the agent and its reports.
#[derive(Clone)]
pub struct Tank {
    pub dry: Arc<AtomicBool>,
    pub metered: Arc<AtomicBool>,
    pub signed_through: SignedThrough,
    /// Whether the vault's account, as last read, names a cluster other than
    /// the one this executor connected to: it is not metered while it does.
    pub other_cluster: Arc<AtomicBool>,
}

/// A vault account naming a cluster other than the one connected to.
#[derive(Debug, PartialEq, Eq)]
pub struct OtherCluster;

impl std::fmt::Display for OtherCluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the vault's account names another cluster than the one connected to"
        )
    }
}

impl std::error::Error for OtherCluster {}

/// The cluster a vault's usage is signed for: the one checked against the
/// node's genesis and the deployment when the executor connected, never the
/// one the node says the vault's account names. A vault whose account names
/// another is refused: a total signed for it could be collected on the
/// cluster it names.
pub fn billed_cluster(connected: [u8; 32], named: &[u8; 32]) -> Result<[u8; 32], OtherCluster> {
    if *named == connected {
        Ok(connected)
    } else {
        Err(OtherCluster)
    }
}

/// Meter one vault and keep its signed total current, as [`crate::usage`]
/// does on an EVM chain.
#[allow(clippy::too_many_arguments)]
pub async fn meter(
    client: SolanaClient,
    platform: Platform,
    key: Arc<Keypair>,
    vault: Pubkey,
    native_usd: Option<f64>,
    totals: Totals,
    tank: Tank,
    running: Arc<AtomicBool>,
    spent: Arc<AtomicI64>,
    ceilings: Ceilings,
) {
    if native_usd.is_none() {
        tracing::warn!(%vault, "this cluster's coin has no price: the lamports the agent spends are not billed");
    }
    let id = (client.cluster.clone(), vault.to_string());
    let connected = client.cluster_value();
    let signed_before = totals
        .lock()
        .ok()
        .and_then(|totals| totals.get(&id).and_then(|signed| signed.total().ok()))
        .unwrap_or_default();
    let gas_now = || spent.load(Ordering::SeqCst).max(0) as u128 * LAMPORT_AS_WEI;
    let mut meter = Meter::new(native_usd, gas_now()).carrying_on(U256::from(signed_before));
    let mut in_force: Option<shared::Allowances> = None;
    let mut since = Instant::now();
    let (mut seconds, mut ran) = (0.0, 0.0);
    let mut last_as_of = 0;
    let mut unread = crate::Throttle::default();
    loop {
        for _ in 0..SAMPLES_PER_TICK {
            tokio::time::sleep(SAMPLE).await;
            let now = Instant::now();
            let passed = now.duration_since(since).as_secs_f64();
            since = now;
            seconds += passed;
            if running.load(Ordering::SeqCst) {
                ran += passed;
            }
        }
        let tick = async {
            let account = platform.vault(&client, &vault).await?;
            // Checked before anything is counted: the time and gas go on
            // accruing, and are billed once the vault reads as this
            // cluster's again.
            let cluster = billed_cluster(connected, &account.cluster)?;
            let slot = client.rpc.slot().await?;
            let found = ceilings.hold(&allowances(&account.allowances));
            meter.add(seconds, ran, in_force.as_ref().unwrap_or(&found), gas_now());
            (seconds, ran) = (0.0, 0.0);
            in_force = Some(found);
            let settled = U256::from(account.tank.usage_settled);
            if meter.agreed(settled, slot, AGREE_BLOCKS, Instant::now()) {
                meter.carry_on_from(settled);
            }
            let total = u64::try_from(meter.total()).unwrap_or(u64::MAX);
            // The cluster's clock, read here: the vault takes a total only
            // as of a moment it has reached, and later than the last.
            let as_of = client.now().await?.max(last_as_of);
            let signed = SignedTotal::sign(&key, &vault, &cluster, total, as_of);
            let dry = total.saturating_sub(account.tank.usage_settled) >= account.tank.balance;
            anyhow::Ok((signed, dry, as_of))
        };
        match tick.await {
            Ok((signed, is_dry, as_of)) => {
                if tank.other_cluster.swap(false, Ordering::SeqCst) {
                    tracing::info!(%vault, "the vault's account names this cluster again; metering it");
                }
                last_as_of = as_of;
                if let Ok(mut totals) = totals.lock() {
                    totals.insert(id.clone(), signed);
                }
                tank.signed_through.signed(since);
                if tank.dry.swap(is_dry, Ordering::SeqCst) != is_dry {
                    if is_dry {
                        tracing::warn!(%vault, "the tank is dry: the agent stops until it is topped up");
                    } else {
                        tracing::info!(%vault, "the tank has money again");
                    }
                }
                tank.metered.store(true, Ordering::SeqCst);
            }
            Err(error) if error.is::<OtherCluster>() => {
                // Nothing is signed, and an agent not yet started is held
                // back, as for a vault not yet metered.
                tank.metered.store(false, Ordering::SeqCst);
                let first = !tank.other_cluster.swap(true, Ordering::SeqCst);
                if first || unread.due(Instant::now()) {
                    tracing::error!(%vault, cluster = %client.cluster, "the vault's account names another cluster than the one connected to: it is not metered, and no total is signed for it");
                }
            }
            Err(error) => {
                if unread.due(Instant::now()) {
                    tracing::warn!(%vault, error = format!("{error:#}"), "could not meter the vault");
                }
            }
        }
    }
}

/// How often a report is sent.
const REPORT_EVERY: Duration = Duration::from_secs(15);

/// A lamport, in the cluster's coin.
const SOL: f64 = 1e9;

/// Send a signed report on the vault every so often, while the serve holds
/// the station's approval with the key and the key is blessed for the vault:
/// the vault is reported on by the key it trades through alone, and nobody
/// would believe a report from a key that isn't blessed.
pub async fn report_on(reporting: Reporting) {
    let mut reporter = Reporter::new(reporting);
    loop {
        tokio::time::sleep(REPORT_EVERY).await;
        reporter.round().await;
    }
}

/// What a vault's reports need.
pub struct Reporting {
    pub client: SolanaClient,
    pub platform: Platform,
    pub key: Arc<Keypair>,
    pub vault: Pubkey,
    pub bundle: [u8; 32],
    pub api: String,
    /// Whether the serve holds the station's approval with the key.
    pub holds: Holds,
    pub counters: Arc<Mutex<Counters>>,
    pub tank: Tank,
    pub totals: Totals,
    pub ceilings: Ceilings,
    pub stake: u64,
}

/// A vault's reports, and what they keep from one to the next.
struct Reporter {
    reporting: Reporting,
    http: reqwest::Client,
    window: std::collections::VecDeque<(Instant, Counters)>,
    unsent: crate::Throttle,
}

impl Reporter {
    fn new(reporting: Reporting) -> Self {
        Self {
            reporting,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            window: std::collections::VecDeque::from([(Instant::now(), Counters::default())]),
            unsent: crate::Throttle::default(),
        }
    }

    /// Send one report, if there is one to send.
    async fn round(&mut self) {
        let now = self
            .reporting
            .counters
            .lock()
            .map(|counters| *counters)
            .unwrap_or_default();
        let hourly = last_hour(&mut self.window, Instant::now(), now);
        let vault = self.reporting.vault;
        let post = match self.post(now, hourly).await {
            Ok(Some(post)) => post,
            Ok(None) => return,
            Err(error) => {
                if self.unsent.due(Instant::now()) {
                    tracing::warn!(%vault, error = format!("{error:#}"), "could not make a report");
                }
                return;
            }
        };
        let sent = self
            .http
            .post(format!(
                "{}/api/{}/health",
                self.reporting.api,
                shared::Chain::Solana
            ))
            .json(&post)
            .send()
            .await
            .context("the report was not delivered")
            .and_then(|answer| {
                answer
                    .error_for_status()
                    .context("the API refused the report")
            });
        if let Err(error) = sent
            && self.unsent.due(Instant::now())
        {
            tracing::warn!(%vault, error = format!("{error:#}"), "the report did not land");
        }
    }

    /// The report to send, if the key is to send one.
    async fn post(&self, now: Counters, hourly: Hourly) -> anyhow::Result<Option<HealthPost>> {
        let Reporting {
            client,
            platform,
            key,
            vault,
            bundle,
            holds,
            tank,
            totals,
            ceilings,
            stake,
            ..
        } = &self.reporting;
        if !holds.holds() {
            return Ok(None);
        }
        let blessed = Blessed::read(client, platform, &key.pubkey(), vault, bundle).await?;
        if !blessed.holds(client.now().await?) {
            return Ok(None);
        }
        let account = platform.vault(client, vault).await?;
        let asked = allowances(&account.allowances);
        let held_below = ceilings.held_below(&asked);
        let daily = shared::Prices::published().daily_cap(&ceilings.hold(&asked));
        let signed = totals
            .lock()
            .ok()
            .and_then(|totals| {
                totals
                    .get(&(client.cluster.clone(), vault.to_string()))
                    .and_then(|signed| signed.total().ok())
            })
            .unwrap_or_default();
        let unpaid = signed.saturating_sub(account.tank.usage_settled);
        let is_dry = tank.dry.load(Ordering::SeqCst);
        let runway = runway(
            U256::from(account.tank.balance),
            U256::from(unpaid),
            daily,
            is_dry,
        )?;
        let lamports = client.lamports(&key.pubkey()).await?;
        let mut health = report(
            now,
            hourly,
            runway,
            now.rejection_rate(),
            key_lamports(lamports, *stake),
            is_dry,
            &held_below,
            key.pubkey().to_string(),
        );
        say_other_cluster(&mut health, tank.other_cluster.load(Ordering::SeqCst));
        let address = vault.to_string();
        let digest = HealthPost::solana_digest(&client.cluster_value(), &address, &health);
        Ok(Some(HealthPost {
            vault: address,
            report: health,
            signature: key.sign_message(&digest).to_string(),
            signer: Some(key.pubkey().to_string()),
        }))
    }
}

/// A vault whose account names another cluster is said in its report as an
/// error: it is not metered, so its bill is not collected.
fn say_other_cluster(health: &mut shared::model::health::Health, other: bool) {
    if other {
        health.entries.push(shared::model::health::Entry {
            at: health.updated,
            level: shared::model::health::Level::Error,
            message: "The vault's account names another cluster than the one the executor is connected to: it is not metered, and no total is signed for it".to_string(),
        });
    }
}

/// The key's lamports: a warning below half its stake, an error below a
/// hundredth of it, which is a few sends.
fn key_lamports(holds: u64, stake: u64) -> Signal {
    Signal {
        name: "Key gas".to_string(),
        value: holds as f64 / SOL,
        unit: "coin".to_string(),
        warn_at: (stake / 2) as f64 / SOL,
        error_at: (stake / 100) as f64 / SOL,
        lower_is_worse: true,
        window: "now".to_string(),
        note: Some(
            "Shonin keeps the key stocked; the agent's fees and rent are billed to the tank as they are spent"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault is reported on by the key it trades through alone: a key
    /// whose serve doesn't hold the station's approval (one waiting to take
    /// a vault over, or one whose vault was let go) makes no report and
    /// doesn't so much as ask the cluster for one; a key whose serve holds
    /// it goes on to read what a report says.
    #[tokio::test]
    async fn only_a_solana_key_whose_serve_holds_the_station_s_approval_reports_on_the_vault() {
        let (client, asked, _cluster) = super::super::counting_cluster().await;
        let holds = Holds::default();
        let reporter = Reporter::new(Reporting {
            client,
            platform: super::super::devnet_platform(),
            key: Arc::new(Keypair::new_from_array([7; 32])),
            vault: Pubkey::new_from_array([0x21; 32]),
            bundle: [5; 32],
            api: "http://127.0.0.1:9".into(),
            holds: holds.clone(),
            counters: Default::default(),
            tank: Tank {
                dry: Default::default(),
                metered: Default::default(),
                signed_through: Default::default(),
                other_cluster: Default::default(),
            },
            totals: Default::default(),
            ceilings: Ceilings::default(),
            stake: 50_000_000,
        });
        let hourly = Hourly::default();

        let made = reporter.post(Counters::default(), hourly).await;
        assert!(matches!(made, Ok(None)), "{made:?}");
        assert_eq!(asked.load(Ordering::SeqCst), 0, "the cluster was asked");

        holds.take();
        assert!(reporter.post(Counters::default(), hourly).await.is_err());
        assert!(
            asked.load(Ordering::SeqCst) > 0,
            "the cluster was not asked"
        );

        holds.let_go();
        let before = asked.load(Ordering::SeqCst);
        let made = reporter.post(Counters::default(), hourly).await;
        assert!(matches!(made, Ok(None)), "{made:?}");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            before,
            "the cluster was asked"
        );
    }

    #[test]
    fn a_vaults_allowances_read_as_the_platforms_own_with_gas_in_dollars() {
        let read = allowances(&Allowed {
            fuel_per_sec: 1,
            memory_mib: 2,
            requests_per_min: 3,
            bytes_per_day: 4,
            streams: 5,
            inference_ms_per_min: 6,
            gas_per_day: 2_500_000,
        });
        assert_eq!(read.fuel_per_sec, 1);
        assert_eq!(read.memory_mib, 2);
        assert_eq!(read.inference_ms_per_min, 6);
        assert_eq!(read.gas_per_day, 2.5);
    }

    /// A vault has the same address on every cluster: a total is signed only
    /// for the cluster connected to, and only for a vault whose account
    /// names it.
    #[test]
    fn a_vault_naming_another_cluster_is_not_metered_and_a_total_is_signed_for_the_connected_one() {
        use chain::solana::programs::common::digest::cluster_value;
        let (localnet, mainnet) = (cluster_value("localnet"), cluster_value("mainnet-beta"));
        assert_eq!(billed_cluster(localnet, &localnet), Ok(localnet));
        assert_eq!(billed_cluster(localnet, &mainnet), Err(OtherCluster));
        assert_eq!(billed_cluster(mainnet, &localnet), Err(OtherCluster));
    }

    #[test]
    fn a_vault_naming_another_cluster_is_said_as_an_error_in_its_report() {
        let blank = || {
            report(
                Counters::default(),
                Default::default(),
                30.0,
                0.0,
                key_lamports(STAKE, STAKE),
                false,
                &[],
                String::new(),
            )
        };
        let mut fine = blank();
        say_other_cluster(&mut fine, false);
        assert!(
            fine.entries
                .iter()
                .all(|entry| entry.level != shared::model::health::Level::Error)
        );
        let mut other = blank();
        say_other_cluster(&mut other, true);
        assert!(other.entries.iter().any(|entry| {
            entry.level == shared::model::health::Level::Error && entry.message.contains("cluster")
        }));
    }

    const STAKE: u64 = 1_000_000_000;

    fn gas_only(per_day: f64) -> shared::Allowances {
        shared::Allowances {
            fuel_per_sec: 0,
            memory_mib: 0,
            requests_per_min: 0,
            bytes_per_day: 0,
            streams: 0,
            inference_ms_per_min: 0,
            gas_per_day: per_day,
        }
    }

    #[test]
    fn lamports_spent_are_billed_at_the_coins_price_within_the_gas_allowance() {
        // One SOL spent at $150, under an allowance of $1,000 a day, a whole
        // day later: $150 billed.
        let lamports: u128 = 1_000_000_000;
        let mut meter = Meter::new(Some(150.0), 0);
        let allowed = gas_only(1_000.0);
        meter.add(86_400.0, 0.0, &allowed, lamports * LAMPORT_AS_WEI);
        assert_eq!(meter.total(), U256::from(150_000_000u64));
        // A tenth of an SOL more under a $10 allowance in a second: held to
        // what that second allowed on top of the day's carry.
        let mut tight = Meter::new(Some(150.0), 0);
        let small = gas_only(10.0);
        tight.add(1.0, 0.0, &small, lamports * LAMPORT_AS_WEI);
        assert!(tight.total() < U256::from(150_000_000u64));
    }
    // Rent the key gets back (an Orca position closed) is credited against
    // what was billed, so an agent that re-centres its band pays one
    // position's rent, not one each time.
    #[test]
    fn rent_paid_refunded_and_paid_again_is_billed_once() {
        // A position's rent (position, Token-2022 mint, its token account),
        // about 0.007 SOL; SOL at $150; a generous allowance.
        let rent: i64 = 7_000_000;
        let allowed = gas_only(1_000.0);
        let mut meter = Meter::new(Some(150.0), 0);
        // The key's running total of lamports spent, `spent` in `Trading`,
        // read as `meter` reads it each tick.
        let gas_now = |spent: i64| spent.max(0) as u128 * LAMPORT_AS_WEI;
        let mut spent = 0i64;
        // A day served first, so the allowance holds back none of it.
        meter.add(86_400.0, 0.0, &allowed, gas_now(spent));
        // Tick 1: an open (rent paid). Tick 2: the close (rent back).
        // Tick 3: the open of the new band (rent paid). Fees left out.
        for change in [rent, -rent, rent] {
            spent += change;
            meter.add(5.0, 0.0, &allowed, gas_now(spent));
        }
        // Net the key is out one position's rent: $1.05.
        let billed = meter.total();
        assert_eq!(
            billed,
            U256::from(1_050_000u64),
            "billed {billed} millionths of a dollar for one position's rent ($1.05) held"
        );
    }
}
