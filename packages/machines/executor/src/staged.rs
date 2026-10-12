//! A close staged at a station through a venue that acts after the
//! transaction (Hyperliquid, Lighter) is proved from the account itself, and
//! the account can't tell the close's fills from anyone else's trades. So
//! from the stage until it has been measured (its part recorded and its
//! dollars asked for, or it delivered or lapsed), or until its deadline has
//! passed, this executor trades nothing at the station's delayed venues: it
//! cancels the account's resting orders there, and refuses every action the
//! agent submits there that would place or change an order or move dollars.
//! Cancels still go. What the account did meanwhile is then the close's own,
//! and the station measures it at the stage's limits.
//!
//! The pause is decided by the chain's clock, never this machine's: once the
//! newest block is past the deadline, no later block can prove the stage,
//! whatever this machine's clock says.

use std::future::Future;
use std::time::Duration;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use chain::Client;
use chain::bindings::station::IStation;

/// How long past a stage's deadline, by the chain's clock, the pause lasts.
/// A Lighter post is stamped with the chain's time at its read of the chain,
/// and the exchange's API may have been read up to two
/// [`POST_READ_SPAN`](crate::robinhood::POST_READ_SPAN)s after that stamp: a
/// post stamped before the deadline may hold what the exchange showed up to
/// then. A third span is room for the chain's clock and this machine's to
/// differ.
pub const PAST_DEADLINE: u64 = 3 * crate::robinhood::POST_READ_SPAN.as_secs();

/// How often a station's stage is looked at for a pause to begin.
pub const LOOK_EVERY: Duration = Duration::from_secs(2);

/// How often, while a pause lasts, the account's resting orders are
/// cancelled again: an order sent just before the stage was seen may come to
/// rest after the first cancel.
pub const CANCEL_AGAIN_EVERY: Duration = Duration::from_secs(30);

/// What of a station's stage the pause turns on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Staged {
    pub staged_at: u64,
    pub swept_block: u64,
    pub proved_by: u64,
}

impl Staged {
    fn read(stage: &chain::bindings::station::Stage) -> Self {
        Self {
            staged_at: stage.stagedAt,
            swept_block: stage.sweptBlock,
            proved_by: stage.provedBy,
        }
    }
}

/// Whether a station's trading at its delayed venues is paused at chain time
/// `now`: a close is staged, not yet measured, and its deadline (with
/// [`PAST_DEADLINE`]) hasn't passed.
pub fn paused(stage: Staged, now: u64) -> bool {
    stage.staged_at != 0
        && stage.swept_block == 0
        && now < stage.proved_by.saturating_add(PAST_DEADLINE)
}

/// Whether `driver` (the name a manifest gives it) is one whose venue acts
/// after the transaction, so a staged close through it pauses its trading.
pub fn delayed(driver: &str) -> bool {
    driver == crate::hyperliquid::DRIVER || driver == crate::robinhood::LIGHTER
}

/// Whether an action the agent submits through `driver` only takes orders
/// off the book: the one kind that goes while a stage pauses the venue. One
/// that doesn't decode is refused as anything else would be.
pub fn only_cancels(driver: &str, payload: &[u8]) -> bool {
    if driver == crate::hyperliquid::DRIVER {
        use ::hyperliquid::payload::Action;
        matches!(
            ::hyperliquid::Trader::decode(payload),
            Ok(Action::Cancel { .. } | Action::CancelAllAt { .. })
        )
    } else if driver == crate::robinhood::LIGHTER {
        use ::lighter::payload::Action;
        matches!(
            strategy_sdk::from_payload::<Action>(payload),
            Ok(Action::Cancel { .. } | Action::CancelAll)
        )
    } else {
        false
    }
}

/// The station's stage, if it pauses trading at its delayed venues now.
pub async fn pause(chain: &Client, station: Address) -> Result<Option<Staged>> {
    let stage = chain
        .read(station, IStation::stageCall {})
        .await
        .context("reading whether a close is staged at the station")?;
    let staged = Staged::read(&stage);
    if staged.staged_at == 0 {
        return Ok(None);
    }
    let (_, now) = chain.head_and_time().await?;
    Ok(paused(staged, now).then_some(staged))
}

/// Why an action that places or changes an order or moves dollars through a
/// delayed venue is refused now, if it is: a stage pauses the venue, or
/// whether one does can't be read (it is refused then, and may be sent
/// again).
pub async fn refusal(chain: &Client, station: Address) -> Option<String> {
    match pause(chain, station).await {
        Ok(None) => None,
        Ok(Some(staged)) => Some(format!(
            "refused: a close staged at the station (at {}) is measured from the account, so nothing but cancels goes there until it is measured or its deadline ({} chain time) has passed",
            staged.staged_at, staged.proved_by
        )),
        Err(error) => Some(format!(
            "refused: whether a close is staged at the station can't be read now ({error:#}), and while one is, nothing but cancels goes there"
        )),
    }
}

/// Whether the account's resting orders are to be cancelled now, for the
/// stage placed at `staged_at`, when they were last cancelled for the stage
/// placed at `last.0`, `last.1` ago: at once for a stage not yet cancelled
/// for, and again every [`CANCEL_AGAIN_EVERY`] while it lasts.
pub fn cancel_due(staged_at: u64, last: Option<(u64, Duration)>) -> bool {
    match last {
        Some((cancelled_for, since)) if cancelled_for == staged_at => since >= CANCEL_AGAIN_EVERY,
        _ => true,
    }
}

/// For as long as the task lives, look at the station's stage every
/// [`LOOK_EVERY`] and, while it pauses the venue, have `cancel` cancel the
/// account's resting orders there (see [`cancel_due`]); `cancel` says how
/// many it cancelled.
pub fn cancel_loop<F, Fut>(
    chain: Client,
    station: Address,
    venue: &'static str,
    cancel: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<usize>> + Send,
{
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(LOOK_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last: Option<(u64, tokio::time::Instant)> = None;
        let mut warned = crate::Throttle::default();
        loop {
            tick.tick().await;
            let staged = match pause(&chain, station).await {
                Ok(Some(staged)) => staged,
                Ok(None) => continue,
                Err(error) => {
                    if warned.due(std::time::Instant::now()) {
                        tracing::warn!(%station, venue, error = %format!("{error:#}"), "whether a close is staged at the station wasn't read");
                    }
                    continue;
                }
            };
            let since = last.map(|(at, when)| (at, when.elapsed()));
            if !cancel_due(staged.staged_at, since) {
                continue;
            }
            match cancel().await {
                Ok(cancelled) => {
                    last = Some((staged.staged_at, tokio::time::Instant::now()));
                    if cancelled != 0 {
                        tracing::info!(%station, venue, cancelled, staged_at = staged.staged_at, "a close is staged at the station: the account's resting orders are cancelled until it is measured");
                    }
                }
                Err(error) => {
                    if warned.due(std::time::Instant::now()) {
                        tracing::warn!(%station, venue, error = %format!("{error:#}"), "a close is staged at the station and the account's resting orders weren't all cancelled: tried again");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one() -> ::hyperliquid::payload::Decimal {
        "1".parse().expect("a decimal")
    }

    const STAGED: Staged = Staged {
        staged_at: 1_000,
        swept_block: 0,
        proved_by: 1_600,
    };

    #[test]
    fn a_station_is_paused_from_its_stage_until_its_deadline_and_a_margin_past_it() {
        assert!(paused(STAGED, 1_000));
        assert!(paused(STAGED, 1_599));
        // A post stamped before the deadline may still hold what the
        // exchange showed after it.
        assert!(paused(STAGED, 1_600));
        assert!(paused(STAGED, 1_600 + PAST_DEADLINE - 1));
        assert!(!paused(STAGED, 1_600 + PAST_DEADLINE));
    }

    #[test]
    fn a_station_with_nothing_staged_is_not_paused() {
        let none = Staged {
            staged_at: 0,
            swept_block: 0,
            proved_by: 0,
        };
        assert!(!paused(none, 0));
        assert!(!paused(none, 1_000));
    }

    #[test]
    fn a_measured_stage_no_longer_pauses_the_station() {
        let measured = Staged {
            swept_block: 7,
            ..STAGED
        };
        assert!(!paused(measured, 1_100));
    }

    #[test]
    fn a_deadline_at_the_end_of_time_does_not_overflow() {
        let far = Staged {
            proved_by: u64::MAX,
            ..STAGED
        };
        assert!(paused(far, u64::MAX - 1));
    }

    #[test]
    fn only_hyperliquid_and_lighter_are_paused_by_a_stage() {
        assert!(delayed(crate::hyperliquid::DRIVER));
        assert!(delayed(crate::robinhood::LIGHTER));
        assert!(!delayed(crate::robinhood::ROBINHOOD));
        assert!(!delayed(crate::solana::ORCA));
        assert!(!delayed(crate::host::LOCAL));
    }

    #[test]
    fn while_paused_only_cancels_go_on_hyperliquid() {
        use ::hyperliquid::payload::{Action, Balance, Order, OrderRef, Side, Tif};
        let encode = |action: &Action| strategy_sdk::to_payload(action);
        let order = Order {
            market: "ETH".into(),
            side: Side::Sell,
            size: one(),
            limit_price: one(),
            tif: Tif::Ioc,
            reduce_only: true,
            client_id: None,
        };
        let hl = crate::hyperliquid::DRIVER;
        assert!(only_cancels(
            hl,
            &encode(&Action::Cancel {
                market: "ETH".into(),
                order: OrderRef::Venue(1),
            })
        ));
        assert!(only_cancels(
            hl,
            &encode(&Action::CancelAllAt { at_ms: Some(1) })
        ));
        assert!(!only_cancels(hl, &encode(&Action::Place(order.clone()))));
        assert!(!only_cancels(
            hl,
            &encode(&Action::Modify {
                order_ref: OrderRef::Venue(1),
                order,
            })
        ));
        assert!(!only_cancels(
            hl,
            &encode(&Action::MoveDollars {
                from: Balance::Perp,
                amount: one(),
            })
        ));
        assert!(!only_cancels(
            hl,
            &encode(&Action::Leverage {
                market: "ETH".into(),
                leverage: 2,
            })
        ));
        assert!(!only_cancels(hl, b"not an action"));
    }

    #[test]
    fn while_paused_only_cancels_go_on_lighter() {
        use ::lighter::payload::{Action, Order, OrderRef, Side, Tif};
        let encode = |action: &Action| strategy_sdk::to_payload(action);
        let lighter = crate::robinhood::LIGHTER;
        assert!(only_cancels(lighter, &encode(&Action::CancelAll)));
        assert!(only_cancels(
            lighter,
            &encode(&Action::Cancel {
                market: "ETH".into(),
                order: OrderRef::Venue(1),
            })
        ));
        assert!(!only_cancels(
            lighter,
            &encode(&Action::Modify {
                market: "ETH".into(),
                order: OrderRef::Venue(1),
                size: one(),
                limit_price: one(),
            })
        ));
        let order = Order {
            market: "ETH".into(),
            side: Side::Sell,
            size: one(),
            limit_price: one(),
            tif: Tif::Ioc,
            reduce_only: true,
            client_id: None,
        };
        assert!(!only_cancels(lighter, &encode(&Action::Place(order))));
        assert!(!only_cancels(lighter, b"not an action"));
    }

    #[test]
    fn resting_orders_are_cancelled_at_once_for_a_new_stage_and_again_while_it_lasts() {
        assert!(cancel_due(1_000, None));
        assert!(!cancel_due(1_000, Some((1_000, Duration::from_secs(1)))));
        assert!(cancel_due(1_000, Some((1_000, CANCEL_AGAIN_EVERY))));
        // Cancelled for an earlier stage: at once for this one.
        assert!(cancel_due(2_000, Some((1_000, Duration::from_secs(1)))));
    }
}
