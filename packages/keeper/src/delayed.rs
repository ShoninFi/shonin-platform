//! Stations whose venue acts after the transaction: a driver whose exit path
//! is `Delayed` (Hyperliquid, where CoreWriter's actions run after the
//! block). Nothing there closes and comes home in one transaction, so the
//! keeper moves each step on in later blocks:
//!
//! - a close is staged by the station for one claim or one holder's units;
//!   the keeper calls `settleClose` once it would go through: to prove,
//!   measure and sweep it, and to deliver it to the vault, which pays whom
//!   it was for, in whole or in part. Every stage ends within its windows.
//!   While one is staged no claim is tried and no units are served (the
//!   vault would refuse them), and the claim a close was last staged for
//!   takes its next turn after the others;
//! - a venue whose withdrawals wait in a pending balance until someone pays
//!   them out (Lighter) has them paid out to the station whenever any is
//!   pending, before the staged close is moved on;
//! - a stopped station's full unwind places what is left to close and sweeps
//!   what earlier closes freed, and reports itself blocked while it ordered
//!   anything: the keeper repeats it until nothing is left at the venue;
//! - money the station's account put into one of the venue's own vaults
//!   (which only its trading key could do) is found through the venue's API
//!   (Hyperliquid's, asked beside the rounds) or the newest post (Lighter's
//!   pools) and reclaimed: counted, brought back when its lock allows, and
//!   trading through that driver ended.
//!
//! Every one of these calls is public and simulated before it is sent.

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result};
use chain::Client;
use chain::bindings::driver::{ExitPath, IDelayedDriver, IDriver};
use chain::bindings::station::IStation;

use crate::{Outcome, WAD, send_outcome, send_roomy_outcome};

/// How often a station's venue is asked whether its account put money into
/// one of the venue's own vaults, and how long a reclaim that went out is
/// given to land before the same vault is looked at again.
const VENUE_VAULTS_EVERY: Duration = Duration::from_secs(60);

/// The longest wait between two stopped unwinds of a delayed station that
/// find nothing changed at the venue.
const MOST_SECONDS_BETWEEN_UNWINDS: u64 = 5 * 60;

/// How many stopped unwinds in a row may find the venue as the last one left
/// it before the keeper stops sending them until something changes there:
/// an unwind that changes nothing is paid for and does nothing.
const MOST_UNWINDS_UNCHANGED: u32 = 6;

/// How long the keeper waits for a venue's API before giving up on it for
/// this look; asked off the vault's round, so a slow API never holds a
/// valuation up.
const VENUE_API_TIMEOUT: Duration = Duration::from_secs(3);

/// One driver of the station's whose venue acts after the transaction: the
/// name the station pinned it under (what its counters and calls use), and
/// the name the implementation gives itself (which venue it is), so the
/// keeper finds a venue whatever name a station pinned it under.
#[derive(Clone, Debug, PartialEq)]
pub struct DelayedDriver {
    pub name: B256,
    pub venue: B256,
    pub implementation: Address,
}

/// What the keeper remembers about one vault's delayed station.
#[derive(Default)]
pub struct Memory {
    /// The station's delayed drivers, read once: a station's drivers are
    /// pinned when it is created. `None` until read.
    drivers: Option<(Address, Vec<DelayedDriver>)>,
    /// When the venue was last asked for the station's venue vaults.
    venue_vaults_at: Option<Instant>,
    /// The last stopped unwind sent, what it found at the venue, and when the
    /// next may go.
    unwinds: Option<Pace>,
}

/// The pace of a delayed station's stopped unwinds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    /// What the venue held when the last one was sent.
    pub seen: B256,
    /// How many slots it was told to wait.
    pub wait: u64,
    /// The first chain time the next may be sent; `u64::MAX` once the venue
    /// has read the same for [`MOST_UNWINDS_UNCHANGED`] unwinds in a row, when
    /// only a change there sends another.
    pub next: u64,
    /// The unwinds in a row that found the venue as the last one left it.
    pub unchanged: u32,
}

impl Pace {
    /// Whether the keeper has stopped unwinding until the venue changes.
    pub fn ended(&self) -> bool {
        self.next == u64::MAX
    }
}

/// The station's delayed drivers, read once and remembered. A station built
/// before delayed drivers existed has none.
pub async fn drivers_of(
    client: &Client,
    station: Address,
    memory: &mut Memory,
) -> Result<Vec<DelayedDriver>> {
    if let Some((at, drivers)) = &memory.drivers
        && *at == station
    {
        return Ok(drivers.clone());
    }
    let spec = client.read(station, IStation::specCall {}).await?;
    let mut delayed = Vec::new();
    for name in spec.drivers {
        let implementation = client
            .read(station, IStation::pinnedDriverCall { driver: name })
            .await?;
        let exit = client
            .read(implementation, IDriver::exitPathCall {})
            .await
            .with_context(|| format!("reading the exit path of {implementation}"))?;
        if matches!(exit, ExitPath::Delayed) {
            let venue = client
                .read(implementation, IDriver::nameCall {})
                .await
                .with_context(|| format!("reading the name of {implementation}"))?;
            delayed.push(DelayedDriver {
                name,
                venue,
                implementation,
            });
        }
    }
    memory.drivers = Some((station, delayed.clone()));
    Ok(delayed)
}

/// Move a staged close on, if one is in progress and the station would take
/// the call now (it is simulated first): once its settle delay has passed, to
/// prove, measure and sweep it (wholly closed, or, its window over, for the
/// part that closed); once its dollars have landed, or a delivery window
/// after the ask, to deliver it, in whole or the part that landed. A call
/// that can't go yet (`NotYet`) is simply tried next round, and nothing is
/// sent.
pub async fn settle_close(client: &Client, keeper: Address, station: Address) -> Result<()> {
    let stage = client.read(station, IStation::stageCall {}).await?;
    let Some(what) = close_step(stage.stagedAt, stage.sweptBlock) else {
        return Ok(());
    };
    if client
        .read(station, IStation::settleCloseCall {})
        .await
        .is_err()
    {
        return Ok(());
    }
    send_outcome(client, keeper, station, IStation::settleCloseCall {}, what).await;
    Ok(())
}

/// What `settleClose` would do for a stage placed at `staged_at` and swept in
/// `swept_block` (zero for none), as the keeper's log says it; `None` with
/// nothing staged, when there is nothing to send.
pub fn close_step(staged_at: u64, swept_block: u64) -> Option<&'static str> {
    match (staged_at, swept_block) {
        (0, _) => None,
        (_, 0) => Some("prove, measure and sweep the staged close, or end it with its window"),
        _ => Some("deliver the staged close, or the part of it that has landed"),
    }
}

alloy::sol! {
    struct LighterPosition {
        uint48 account;
        uint16 market;
        int64 size;
    }
    struct LighterPrice {
        uint16 market;
        uint32 mark;
        uint32 index;
        uint32 bid;
        uint32 ask;
    }
    struct LighterShares {
        uint48 account;
        uint48 pool;
        uint64 shares;
        uint256 value;
    }
    struct LighterReport {
        uint64 asOf;
        uint48[] accounts;
        uint256 equity;
        uint256 pending;
        uint256 inFlight;
        uint256 arrived;
        uint256 sent;
        uint256[] available;
        LighterPosition[] positions;
        LighterPrice[] prices;
        LighterShares[] pools;
    }
    interface ILighterDriver {
        function exchange() external view returns (address);
        function DOLLAR_ASSET() external view returns (uint16);
        function board() external view returns (address);
    }
    interface ILighterBoard {
        function reportOf(address station) external view returns (LighterReport memory report, bool exists);
    }
    interface ILighterExchange {
        function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
        function withdrawPendingBalance(address owner, uint16 assetIndex, uint128 baseAmount) external;
    }
}

/// Collect what a venue has paid out to the station but holds until someone
/// asks for it: Lighter's pending balance, which its withdrawals land in
/// minutes after they are asked for, and which anyone may send on to its
/// owner. Every other venue sends its withdrawals itself.
pub async fn collect_withdrawals(
    client: &Client,
    keeper: Address,
    station: Address,
    drivers: &[DelayedDriver],
) -> Result<()> {
    for driver in drivers {
        if driver.venue != name_word("lighter") {
            continue;
        }
        let exchange = client
            .read(driver.implementation, ILighterDriver::exchangeCall {})
            .await?;
        let asset = client
            .read(driver.implementation, ILighterDriver::DOLLAR_ASSETCall {})
            .await?;
        let pending = client
            .read(
                exchange,
                ILighterExchange::getPendingBalanceCall {
                    owner: station,
                    assetIndex: asset,
                },
            )
            .await?;
        if pending == 0 {
            continue;
        }
        let call = ILighterExchange::withdrawPendingBalanceCall {
            owner: station,
            assetIndex: asset,
            baseAmount: pending,
        };
        if client.read(exchange, call.clone()).await.is_err() {
            continue;
        }
        send_outcome(
            client,
            keeper,
            exchange,
            call,
            "collect the station's pending withdrawal",
        )
        .await;
    }
    Ok(())
}

/// What a stopped delayed station's full unwind would do now, from views
/// alone (a simulation of it can't show what the venue does after the
/// block).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Work {
    /// Some delayed driver has positions left to order closed, inside the
    /// stopped station's band.
    pub orders: bool,
    /// Some delayed driver has dollars free at the venue to send home.
    pub sweep: bool,
    /// What the unwind would send home from the station itself now.
    pub sent_home: U256,
    /// Whether a market outside the band holds some of it back.
    pub outside: bool,
    /// A digest of what the venue holds, to tell an unwind that changed
    /// nothing from one that did.
    pub seen: B256,
}

/// Whether a stopped delayed station's unwind is worth sending: it has
/// closing orders to place, dollars at the venue to bring home, or dollars
/// at the station worth an unwind of their own.
pub fn unwind_wanted(work: &Work, worth_sending_home: bool) -> bool {
    work.orders || work.sweep || worth_sending_home
}

/// Whether the next stopped unwind may go now, and the pace after it. One a
/// slot while the venue keeps changing (orders filling, dollars landing);
/// when the venue reads the same as at the last one (a sweep the venue keeps
/// dropping, an order it keeps refusing, a stale post's orders), twice as
/// long each time, up to [`MOST_SECONDS_BETWEEN_UNWINDS`], and after
/// [`MOST_UNWINDS_UNCHANGED`] such unwinds in a row none at all until the
/// venue reads differently.
pub fn pace(before: Option<&Pace>, seen: B256, now: u64, slot_interval: u64) -> Option<Pace> {
    let interval = slot_interval.max(1);
    let most = (MOST_SECONDS_BETWEEN_UNWINDS / interval).max(1);
    match before {
        Some(before) if before.ended() && before.seen == seen => None,
        Some(before) if !before.ended() && now < before.next => None,
        Some(before) if before.seen == seen => {
            let unchanged = before.unchanged + 1;
            let wait = (before.wait * 2).clamp(1, most);
            Some(Pace {
                seen,
                wait,
                next: if unchanged >= MOST_UNWINDS_UNCHANGED {
                    u64::MAX
                } else {
                    now + wait * interval
                },
                unchanged,
            })
        }
        _ => Some(Pace {
            seen,
            wait: 1,
            next: now + interval,
            unchanged: 0,
        }),
    }
}

/// Read what a stopped delayed station's unwind would do at chain time
/// `now`.
pub async fn work(
    client: &Client,
    station: Address,
    drivers: &[DelayedDriver],
    now: u64,
) -> Result<Work> {
    let band = client.read(station, IStation::stoppedBandCall {}).await?;
    let simulated = client
        .read(
            station,
            IStation::unwindCall {
                fraction: U256::from(WAD),
            },
        )
        .await
        .context("simulating the unwind")?;
    let mut work = Work {
        sent_home: simulated.sentHome,
        ..Work::default()
    };
    let mut seen = Vec::new();
    for driver in drivers {
        // The unwind places no order and sweeps nothing through a driver
        // before it may be moved through again, or whose close is blocked.
        let movable_at = client
            .read(
                driver.implementation,
                IDelayedDriver::movableAtCall { station },
            )
            .await?;
        let movable = movable_at <= U256::from(now);
        let closing = client
            .read(
                driver.implementation,
                IDriver::closeCall {
                    station,
                    fraction: U256::from(WAD),
                    bandBps: band,
                },
            )
            .await?;
        if closing.blocked {
            work.outside = true;
        } else if movable && !closing.calls.is_empty() {
            work.orders = true;
        }
        // The orders themselves: a band that has widened since names new
        // limits, which is a change worth an unwind.
        let close_data: Vec<Bytes> = closing.calls.into_iter().map(|call| call.data).collect();
        let sweeping = client
            .read(
                driver.implementation,
                IDriver::sweepCall {
                    station,
                    amount: U256::MAX,
                },
            )
            .await?;
        if movable && !closing.blocked && !sweeping.is_empty() {
            work.sweep = true;
        }
        let exposure = client
            .read(
                driver.implementation,
                IDelayedDriver::exposureCall { station },
            )
            .await?;
        let sweep_data: Vec<Bytes> = sweeping.into_iter().map(|call| call.data).collect();
        seen.push(
            (
                driver.name,
                exposure,
                sweep_data,
                close_data,
                closing.blocked,
            )
                .abi_encode(),
        );
    }
    seen.push(work.sent_home.abi_encode());
    work.seen = keccak256(seen.concat());
    Ok(work)
}

/// A stopped delayed station's full unwind, repeated until nothing is left
/// at the venue, at the pace [`pace`] sets. `worth` says whether an amount
/// sent home from the station is worth an unwind of its own.
#[allow(clippy::too_many_arguments)]
pub async fn unwind_stopped(
    client: &Client,
    keeper: Address,
    vault: Address,
    station: Address,
    drivers: &[DelayedDriver],
    now: u64,
    slot_interval: u64,
    worth: impl Fn(U256) -> bool,
    memory: &mut Memory,
) -> Result<()> {
    if memory
        .unwinds
        .as_ref()
        .is_some_and(|before| !before.ended() && now < before.next)
    {
        return Ok(());
    }
    collect_withdrawals(client, keeper, station, drivers).await?;
    let work = work(client, station, drivers, now).await?;
    if !unwind_wanted(&work, worth(work.sent_home)) {
        if work.outside {
            tracing::debug!(%vault, "the stopped station's markets are outside its band; waiting");
        }
        return Ok(());
    }
    let Some(next) = pace(memory.unwinds.as_ref(), work.seen, now, slot_interval) else {
        return Ok(());
    };
    let outcome = send_roomy_outcome(
        client,
        keeper,
        station,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        "unwind the delayed station",
    )
    .await;
    // One that never reached the chain tried nothing: it doesn't set the
    // pace.
    if outcome.mined() {
        memory.unwinds = Some(next);
    }
    if outcome == Outcome::Done {
        tracing::info!(%vault, orders = work.orders, sweep = work.sweep, "unwound the stopped station a step");
    }
    Ok(())
}

/// One of the venue's own vaults the station's account holds money in.
#[derive(Clone, Debug, PartialEq)]
pub struct VenueVault {
    pub address: Address,
    /// Dollars, as the venue writes them.
    pub equity: String,
}

/// Read the venue's answer to `userVaultEquities`: each vault with money in
/// it. Entries that don't read are left out, so a change in the answer's
/// shape finds nothing rather than something wrong; what is reclaimed is
/// read again on-chain before anything is sent.
pub fn venue_vaults_from(answer: &serde_json::Value) -> Vec<VenueVault> {
    let Some(entries) = answer.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let address = entry.get("vaultAddress")?.as_str()?.parse().ok()?;
            let equity = entry.get("equity")?.as_str()?.to_string();
            let positive = equity
                .parse::<f64>()
                .is_ok_and(|equity| equity.is_finite() && equity > 0.0);
            positive.then_some(VenueVault { address, equity })
        })
        .collect()
}

/// The venue's API for a driver (by the name its implementation gives
/// itself) on a chain, if the keeper knows it.
pub fn venue_api(driver: B256, chain_id: u64) -> Option<&'static str> {
    if driver != name_word("hyperliquid") {
        return None;
    }
    match chain_id {
        999 => Some("https://api.hyperliquid.xyz/info"),
        998 => Some("https://api.hyperliquid-testnet.xyz/info"),
        _ => None,
    }
}

/// A driver's name as the contracts store it.
pub fn name_word(name: &str) -> B256 {
    let mut word = [0u8; 32];
    word[..name.len()].copy_from_slice(name.as_bytes());
    B256::from(word)
}

/// Whether to send `reclaim` for one venue vault: the first time the station
/// is seen holding money there, and after that whenever the driver has calls
/// to bring it back (its lock is over).
pub fn reclaim_wanted(listed: bool, equity: U256, calls: usize) -> bool {
    (!listed && equity != U256::ZERO) || (listed && calls != 0)
}

/// The pools a Lighter post names the station's accounts holding shares in,
/// each pool's index as the station and the driver name it: as an address.
pub fn pools_from(report: &LighterReport) -> Vec<Address> {
    let mut pools = Vec::new();
    for shares in &report.pools {
        if shares.shares == 0 {
            continue;
        }
        let index: u64 = shares.pool.to();
        let mut bytes = [0u8; 20];
        bytes[12..].copy_from_slice(&index.to_be_bytes());
        let pool = Address::from(bytes);
        if !pools.contains(&pool) {
            pools.push(pool);
        }
    }
    pools
}

/// What a venue's API last said about a station's venue vaults, and whether
/// a look is out now.
#[derive(Default)]
struct Looked {
    at: Option<Instant>,
    vaults: Vec<Address>,
    asking: bool,
}

/// The venue vaults each station was last found holding money in, by API
/// and station, filled in by looks that run beside the rounds.
static VENUE_VAULTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(&'static str, Address), Looked>>,
> = std::sync::LazyLock::new(Default::default);

/// The venue vaults the API last named for `station`, and a new look sent
/// off beside the round if the last is older than [`VENUE_VAULTS_EVERY`].
/// Never waits for the API: a slow or hanging one costs no vault its
/// valuation window, only the freshness of this list.
fn venue_vaults_known(http: &reqwest::Client, url: &'static str, station: Address) -> Vec<Address> {
    let mut looked = VENUE_VAULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = looked.entry((url, station)).or_default();
    let stale = entry.at.is_none_or(|at| at.elapsed() >= VENUE_VAULTS_EVERY);
    if stale && !entry.asking {
        entry.asking = true;
        let http = http.clone();
        tokio::spawn(async move {
            let found =
                tokio::time::timeout(VENUE_API_TIMEOUT, ask_venue_vaults(&http, url, station))
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|found| found);
            let mut looked = VENUE_VAULTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = looked.entry((url, station)).or_default();
            entry.asking = false;
            entry.at = Some(Instant::now());
            match found {
                Ok(vaults) => entry.vaults = vaults,
                Err(error) => {
                    tracing::debug!(%station, error = format!("{error:#}"), "the venue's API did not say where the station's money is");
                }
            }
        });
    }
    entry.vaults.clone()
}

async fn ask_venue_vaults(
    http: &reqwest::Client,
    url: &str,
    station: Address,
) -> Result<Vec<Address>> {
    let answer: serde_json::Value = http
        .post(url)
        .timeout(VENUE_API_TIMEOUT)
        .json(&serde_json::json!({
            "type": "userVaultEquities",
            "user": format!("{station:#x}"),
        }))
        .send()
        .await
        .context("asking the venue for the station's vaults")?
        .error_for_status()?
        .json()
        .await?;
    Ok(venue_vaults_from(&answer)
        .into_iter()
        .map(|vault| vault.address)
        .collect())
}

/// Find money the station's account put into the venue's own vaults, and
/// reclaim it: on Hyperliquid from what its API last said (asked beside the
/// rounds, never waited for), on Lighter from the pools the newest post
/// names, and in either case every vault already listed. Looked at once a
/// minute per station.
pub async fn reclaim(
    client: &Client,
    http: &reqwest::Client,
    keeper: Address,
    station: Address,
    drivers: &[DelayedDriver],
    memory: &mut Memory,
) -> Result<()> {
    if memory
        .venue_vaults_at
        .is_some_and(|at| at.elapsed() < VENUE_VAULTS_EVERY)
    {
        return Ok(());
    }
    memory.venue_vaults_at = Some(Instant::now());
    for driver in drivers {
        let mut candidates = if let Some(url) = venue_api(driver.venue, client.chain_id) {
            venue_vaults_known(http, url, station)
        } else if driver.venue == name_word("lighter") {
            let board = client
                .read(driver.implementation, ILighterDriver::boardCall {})
                .await?;
            let post = client
                .read(board, ILighterBoard::reportOfCall { station })
                .await?;
            if post.exists {
                pools_from(&post.report)
            } else {
                Vec::new()
            }
        } else {
            continue;
        };
        let listed = client
            .read(
                station,
                IStation::heldInCall {
                    driver: driver.name,
                },
            )
            .await?;
        for held in &listed {
            if !candidates.contains(held) {
                candidates.push(*held);
            }
        }
        for venue_vault in candidates {
            let (calls, equity) = {
                let answer = client
                    .read(
                        driver.implementation,
                        IDelayedDriver::reclaimCall {
                            station,
                            held: venue_vault,
                        },
                    )
                    .await?;
                (answer.calls.len(), answer.equity)
            };
            if !reclaim_wanted(listed.contains(&venue_vault), equity, calls) {
                continue;
            }
            if client
                .read(
                    station,
                    IStation::reclaimCall {
                        driver: driver.name,
                        venueVault: venue_vault,
                    },
                )
                .await
                .is_err()
            {
                continue;
            }
            tracing::warn!(%station, %venue_vault, %equity, "the station's account holds money in one of the venue's vaults; reclaiming it");
            send_outcome(
                client,
                keeper,
                station,
                IStation::reclaimCall {
                    driver: driver.name,
                    venueVault: venue_vault,
                },
                "reclaim money from a venue vault",
            )
            .await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_close_is_moved_on_only_while_one_is_staged() {
        assert_eq!(close_step(0, 0), None, "nothing staged: nothing to send");
        assert_eq!(close_step(0, 9), None);
        assert!(close_step(100, 0).unwrap().starts_with("prove"));
        assert!(close_step(100, 7).unwrap().starts_with("deliver"));
    }

    #[test]
    fn a_stopped_delayed_station_is_unwound_while_anything_is_left_at_the_venue() {
        let nothing = Work::default();
        assert!(!unwind_wanted(&nothing, false));
        // Positions left to order closed.
        let orders = Work {
            orders: true,
            ..Work::default()
        };
        assert!(unwind_wanted(&orders, false));
        // Dollars freed by earlier closes, still at the venue.
        let sweep = Work {
            sweep: true,
            ..Work::default()
        };
        assert!(unwind_wanted(&sweep, false));
        // Dollars landed at the station, worth sending home.
        assert!(unwind_wanted(&nothing, true));
        // A market outside the band alone is no reason to send anything.
        let outside = Work {
            outside: true,
            ..Work::default()
        };
        assert!(!unwind_wanted(&outside, false));
    }

    #[test]
    fn a_delayed_unwind_goes_once_a_slot_while_the_venue_changes() {
        let a = B256::repeat_byte(1);
        let b = B256::repeat_byte(2);
        let first = pace(None, a, 1_000, 10).expect("the first goes at once");
        assert_eq!(first.next, 1_010);
        // Not again inside the slot.
        assert_eq!(pace(Some(&first), b, 1_005, 10), None);
        // Next slot, the venue changed: one slot's wait again.
        let second = pace(Some(&first), b, 1_010, 10).unwrap();
        assert_eq!(second.wait, 1);
        assert_eq!(second.next, 1_020);
    }

    #[test]
    fn a_delayed_unwind_that_changes_nothing_backs_off_and_then_stops_until_the_venue_changes() {
        let same = B256::repeat_byte(7);
        let mut now = 0;
        let mut before = pace(None, same, now, 10).unwrap();
        let mut waits = Vec::new();
        while !before.ended() {
            now = before.next;
            before = pace(Some(&before), same, now, 10).unwrap();
            waits.push(before.wait);
        }
        assert_eq!(waits, vec![2, 4, 8, 16, 30, 30]);
        // Ended: however long after, nothing while the venue reads the same.
        assert_eq!(pace(Some(&before), same, now + 1_000_000, 10), None);
        // A change at the venue brings it back to every slot.
        let changed = pace(Some(&before), B256::repeat_byte(8), now + 5, 10).unwrap();
        assert_eq!(changed.wait, 1);
        assert_eq!(changed.next, now + 15);
        assert!(!changed.ended());
    }

    #[test]
    fn the_pools_a_post_names_are_the_ones_reclaimed() {
        use alloy::primitives::aliases::U48;
        let shares = |pool: u64, shares: u64| LighterShares {
            account: U48::from(7u64),
            pool: U48::from(pool),
            shares,
            value: U256::from(1u64),
        };
        let report = LighterReport {
            asOf: 0,
            accounts: vec![],
            equity: U256::ZERO,
            pending: U256::ZERO,
            inFlight: U256::ZERO,
            arrived: U256::ZERO,
            sent: U256::ZERO,
            available: vec![],
            positions: vec![],
            prices: vec![],
            pools: vec![
                shares(281_474_976_710_654, 5),
                shares(3, 0),
                shares(281_474_976_710_654, 1),
            ],
        };
        let pools = pools_from(&report);
        assert_eq!(
            pools.len(),
            1,
            "a pool with no shares is left out, and each pool named once"
        );
        assert_eq!(
            pools[0],
            "0x0000000000000000000000000000fffffffffffe"
                .parse::<Address>()
                .unwrap()
        );
    }

    #[test]
    fn only_vaults_with_money_in_them_are_read_from_the_venue_s_answer() {
        let answer = serde_json::json!([
            {"vaultAddress": "0xa15099a30bbf2e68942d6f4c43d70d04faeab0a0", "equity": "12.5"},
            {"vaultAddress": "0x1111111111111111111111111111111111111111", "equity": "0.0"},
            {"vaultAddress": "not an address", "equity": "3"},
            {"vaultAddress": "0x2222222222222222222222222222222222222222", "equity": "NaN"},
            {"equity": "5"},
        ]);
        let vaults = venue_vaults_from(&answer);
        assert_eq!(vaults.len(), 1);
        assert_eq!(
            vaults[0].address,
            "0xa15099a30bbf2e68942d6f4c43d70d04faeab0a0"
                .parse::<Address>()
                .unwrap()
        );
        assert!(venue_vaults_from(&serde_json::json!({"error": "x"})).is_empty());
    }

    #[test]
    fn a_venue_vault_is_reclaimed_when_first_found_and_again_once_its_lock_is_over() {
        let some = U256::from(5_000_000u64);
        // First found holding money: listed, counted, trading ended.
        assert!(reclaim_wanted(false, some, 0));
        // Found empty and never listed: nothing to do.
        assert!(!reclaim_wanted(false, U256::ZERO, 0));
        // Listed and still locked: nothing yet.
        assert!(!reclaim_wanted(true, some, 0));
        // Listed and the lock is over: bring it back.
        assert!(reclaim_wanted(true, some, 1));
    }

    #[test]
    fn only_hyperliquid_s_api_is_asked_and_only_on_its_own_chains() {
        let hyperliquid = name_word("hyperliquid");
        assert_eq!(
            venue_api(hyperliquid, 998),
            Some("https://api.hyperliquid-testnet.xyz/info")
        );
        assert_eq!(
            venue_api(hyperliquid, 999),
            Some("https://api.hyperliquid.xyz/info")
        );
        assert_eq!(venue_api(hyperliquid, 84_532), None);
        assert_eq!(venue_api(name_word("local"), 998), None);
    }
}
