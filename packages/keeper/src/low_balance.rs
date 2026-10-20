//! The keeper's own account on each chain, said at warn when it runs low.
//!
//! The keeper pays from one account a chain: its own sends, the gas it
//! stakes vault keys and the services' keys with, and the blessings it
//! relays. Nothing tops that account up but a person, so once it holds less
//! than a week's spending at its recent rate, or less than the chain's floor
//! (`keeper_balance_floor_micro`), whichever is higher, the keeper says so
//! at warn, at most every [`SAY_EVERY`] while it stays low, and at info once
//! it is topped up past that line again.
//!
//! The rate is the Testnet watch's: the account's fall since its last
//! top-up, judged once that spans [`MIN_SPAN`], from balances read every
//! [`LOOK_EVERY`] and kept for [`KEEP`]. The watch judges the same week from
//! outside, so the two say the same thing about the same account.

use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use anyhow::Result;

/// How often the account's balance is read.
pub const LOOK_EVERY: Duration = Duration::from_secs(10 * 60);

/// What the account must hold beyond: this long of its spending.
pub const RUNWAY: u64 = 7 * 86_400;

/// The shortest span a rate is judged over, in seconds.
pub const MIN_SPAN: u64 = 12 * 3_600;

/// How long a balance read is kept, in seconds.
pub const KEEP: u64 = 8 * 86_400;

/// How often a low account is said again while it stays low, in seconds.
pub const SAY_EVERY: u64 = 6 * 3_600;

/// One account's balances, read over time, and when it was last said low.
#[derive(Debug, Default)]
pub struct Runway {
    /// Each balance read, oldest first: the time in seconds and the balance
    /// in the coin's smallest unit.
    read: VecDeque<(u64, u128)>,
    said: Option<u64>,
}

/// What one balance read says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    /// Nothing new.
    Nothing,
    /// Low, and due to be said: what it holds, the line it is under, and its
    /// spending a day at its recent rate, if that is known yet.
    Low {
        balance: u128,
        line: u128,
        per_day: Option<u128>,
    },
    /// Back past the line after it was said low.
    Enough,
}

impl Runway {
    /// Note the balance read at `now` (seconds), against a floor of `floor`,
    /// and say what is due.
    pub fn note(&mut self, now: u64, balance: u128, floor: u128) -> Said {
        while self
            .read
            .front()
            .is_some_and(|(at, _)| now.saturating_sub(*at) > KEEP)
        {
            self.read.pop_front();
        }
        self.read.push_back((now, balance));
        let per_day = self.per_day();
        let week = per_day.map_or(0, |per_day| per_day.saturating_mul(RUNWAY as u128 / 86_400));
        let line = floor.max(week);
        if balance >= line {
            return if self.said.take().is_some() {
                Said::Enough
            } else {
                Said::Nothing
            };
        }
        if self
            .said
            .is_some_and(|said| now.saturating_sub(said) < SAY_EVERY)
        {
            return Said::Nothing;
        }
        self.said = Some(now);
        Said::Low {
            balance,
            line,
            per_day,
        }
    }

    /// The account's spending a day: its fall from the oldest balance read
    /// since its last top-up to the newest, once that spans [`MIN_SPAN`]; a
    /// rise between two reads is a top-up, and the span starts after it.
    fn per_day(&self) -> Option<u128> {
        let &(now, balance) = self.read.back()?;
        let (mut oldest, mut newer) = ((now, balance), balance);
        for &(at, value) in self.read.iter().rev().skip(1) {
            if value < newer {
                break;
            }
            oldest = (at, value);
            newer = value;
        }
        let span = now.saturating_sub(oldest.0);
        if span < MIN_SPAN {
            return None;
        }
        let fall = oldest.1.saturating_sub(balance);
        Some(fall.saturating_mul(86_400) / u128::from(span))
    }
}

/// `amount` of a coin with `decimals` decimals, written in whole coins.
pub fn coins(amount: u128, decimals: u32) -> String {
    let one = 10u128.pow(decimals);
    let fraction = format!("{:0width$}", amount % one, width = decimals as usize);
    let fraction = fraction[..6.min(fraction.len())].trim_end_matches('0');
    if fraction.is_empty() {
        format!("{}", amount / one)
    } else {
        format!("{}.{fraction}", amount / one)
    }
}

/// A floor of `micro` millionths of a coin with `decimals` decimals, in its
/// smallest unit.
pub fn floor_of(micro: u64, decimals: u32) -> u128 {
    let micro = u128::from(micro);
    if decimals >= 6 {
        micro.saturating_mul(10u128.pow(decimals - 6))
    } else {
        micro / 10u128.pow(6 - decimals)
    }
}

/// Watch the keeper's account on `chain` for as long as the keeper runs:
/// `read` gives its balance in the coin's smallest unit (`decimals`
/// decimals), every [`LOOK_EVERY`], and `floor_micro` is the chain's floor.
pub async fn watch<F, Fut>(chain: String, account: String, decimals: u32, floor_micro: u64, read: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<u128>>,
{
    let floor = floor_of(floor_micro, decimals);
    let mut runway = Runway::default();
    loop {
        match read().await {
            Ok(balance) => match runway.note(wall_seconds(), balance, floor) {
                Said::Nothing => {}
                Said::Low {
                    balance,
                    line,
                    per_day,
                } => tracing::warn!(
                    %chain,
                    %account,
                    balance = coins(balance, decimals),
                    line = coins(line, decimals),
                    per_day = per_day.map(|per_day| coins(per_day, decimals)),
                    floor = coins(floor, decimals),
                    "the keeper's account is low: it holds less than a week's spending at its \
                     recent rate, or less than the chain's floor; top it up from the funder"
                ),
                Said::Enough => tracing::info!(
                    %chain,
                    %account,
                    balance = coins(balance, decimals),
                    "the keeper's account holds enough again"
                ),
            },
            Err(error) => tracing::debug!(
                %chain,
                error = format!("{error:#}"),
                "the keeper's balance could not be read"
            ),
        }
        tokio::time::sleep(LOOK_EVERY).await;
    }
}

fn wall_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;

    #[test]
    fn an_account_under_its_floor_is_said_low_before_any_rate_is_known() {
        let mut runway = Runway::default();
        assert_eq!(
            runway.note(0, 90, 100),
            Said::Low {
                balance: 90,
                line: 100,
                per_day: None
            }
        );
        assert_eq!(runway.note(HOUR, 200, 100), Said::Enough);
        assert_eq!(runway.note(2 * HOUR, 200, 100), Said::Nothing);
    }

    #[test]
    fn an_account_holding_less_than_a_week_at_its_rate_is_said_low_above_its_floor() {
        let mut runway = Runway::default();
        // A thousand a day: a week's spending is 7,000.
        let mut said = Vec::new();
        for hour in 0..=24 {
            let balance = 10_000 - u128::from(hour) * 1_000 / 24;
            said.push(runway.note(hour * HOUR, balance, 100));
        }
        // Nothing until the rate spans twelve hours; 9,500 left is more than
        // a week's 7,000.
        assert!(said.iter().all(|said| *said == Said::Nothing), "{said:?}");
        // At 6,900, the week's 7,000 is past it.
        let mut runway = Runway::default();
        runway.note(0, 9_900, 100);
        assert_eq!(
            runway.note(3 * DAY, 6_900, 100),
            Said::Low {
                balance: 6_900,
                line: 7_000,
                per_day: Some(1_000)
            }
        );
    }

    #[test]
    fn a_low_account_is_said_again_only_every_six_hours() {
        let mut runway = Runway::default();
        assert!(matches!(runway.note(0, 50, 100), Said::Low { .. }));
        assert_eq!(runway.note(HOUR, 50, 100), Said::Nothing);
        assert_eq!(runway.note(SAY_EVERY - 1, 50, 100), Said::Nothing);
        assert!(matches!(runway.note(SAY_EVERY, 50, 100), Said::Low { .. }));
    }

    #[test]
    fn a_top_up_starts_the_rate_again() {
        let mut runway = Runway::default();
        // Spent 1,000 a day for two days, then topped up.
        runway.note(0, 10_000, 0);
        runway.note(2 * DAY, 8_000, 0);
        runway.note(2 * DAY + HOUR, 20_000, 0);
        // Only an hour since the top-up: no rate yet, so no week's line.
        assert_eq!(runway.read.len(), 3);
        assert_eq!(runway.per_day(), None);
        // A day on, at 100 a day.
        assert_eq!(runway.note(3 * DAY + HOUR, 19_900, 0), Said::Nothing);
        assert_eq!(runway.per_day(), Some(100));
    }

    #[test]
    fn balances_older_than_eight_days_are_let_go() {
        let mut runway = Runway::default();
        runway.note(0, 10_000, 0);
        runway.note(KEEP + 1, 9_000, 0);
        assert_eq!(runway.read.len(), 1);
    }

    #[test]
    fn a_floor_and_a_balance_read_in_whole_coins() {
        assert_eq!(floor_of(20_000, 18), 20_000_000_000_000_000);
        assert_eq!(floor_of(200_000, 9), 200_000_000);
        assert_eq!(floor_of(1_500_000, 6), 1_500_000);
        assert_eq!(coins(20_000_000_000_000_000, 18), "0.02");
        assert_eq!(coins(1_890_575_520, 9), "1.890575");
        assert_eq!(coins(3_000_000_000, 9), "3");
    }
}
