//! Every figure on the site goes through these (Foundations, "Numbers and
//! formats"). No web dependencies, so the pre-render and the tests use exactly
//! what the app shows.
//!
//! - Money: dollars with separators; cents on what a person owns or moves,
//!   whole dollars for vault totals. Never abbreviated.
//! - Share price: four decimals.
//! - Returns: one decimal, always signed, a true minus (U+2212); zero is
//!   0.0% with no sign.
//! - Times: 24-hour with the UTC suffix; dates as a short month and day, the
//!   year only when it isn't this year.
//! - Hashes and addresses: `0x` and the first 4 … last 4.

use time::{Date, Month, PrimitiveDateTime, Weekday};

/// The true minus sign, which lines up with digits in tabular figures.
pub const MINUS: char = '\u{2212}';
/// What a missing figure shows, with the reason beside it.
pub const MISSING: &str = "—";

/// Group the integer part in threes: 2418330 becomes 2,418,330.
fn grouped(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A non-negative number with separators and a fixed number of decimals.
fn unsigned(value: f64, decimals: usize) -> String {
    let rounded = format!("{:.*}", decimals, value.abs());
    match rounded.split_once('.') {
        Some((whole, fraction)) => format!("{}.{fraction}", grouped(whole)),
        None => grouped(&rounded),
    }
}

/// Whether a value rounds to zero at this many decimals, so it can't be shown
/// with a sign ("−0.0%" would be a lie).
fn rounds_to_zero(value: f64, decimals: usize) -> bool {
    format!("{:.*}", decimals, value.abs())
        .chars()
        .all(|c| c == '0' || c == '.')
}

/// A number with separators and a fixed number of decimals, a true minus when
/// negative: 8,771.4.
pub fn number(value: f64, decimals: usize) -> String {
    if value < 0.0 && !rounds_to_zero(value, decimals) {
        format!("{MINUS}{}", unsigned(value, decimals))
    } else {
        unsigned(value, decimals)
    }
}

fn money_with(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return MISSING.to_string();
    }
    if value < 0.0 && !rounds_to_zero(value, decimals) {
        format!("{MINUS}${}", unsigned(value, decimals))
    } else {
        format!("${}", unsigned(value, decimals))
    }
}

/// A vault total, to the dollar: $2,418,330.
pub fn money_whole(value: f64) -> String {
    money_with(value, 0)
}

/// Money a person owns or moves, to the cent: $10,001.25.
pub fn money(value: f64) -> String {
    money_with(value, 2)
}

/// A share price, to four decimals: $1.1402. Also the high-water mark.
pub fn price(value: f64) -> String {
    money_with(value, 4)
}

/// A return, from a fraction: one decimal, always signed, a true minus. Zero
/// (after rounding) is 0.0% with no sign.
pub fn ret(fraction: f64) -> String {
    if !fraction.is_finite() {
        return MISSING.to_string();
    }
    let percent = fraction * 100.0;
    if rounds_to_zero(percent, 1) {
        "0.0%".to_string()
    } else if percent > 0.0 {
        format!("+{}%", unsigned(percent, 1))
    } else {
        format!("{MINUS}{}%", unsigned(percent, 1))
    }
}

/// Which way a figure went, for its colour: gain, loss or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Flat,
}

/// The direction of a return as it will be shown: a value that rounds to
/// 0.0% is flat, whatever its sign.
pub fn direction(fraction: f64) -> Direction {
    let percent = fraction * 100.0;
    if !percent.is_finite() || rounds_to_zero(percent, 1) {
        Direction::Flat
    } else if percent > 0.0 {
        Direction::Up
    } else {
        Direction::Down
    }
}

/// A count of shares: 8,771.4 shares.
pub fn shares(value: f64) -> String {
    format!("{} shares", number(value, 1))
}

/// A rate in basis points, as the docs write it: 10 is 0.1%, 100 is 1%.
pub fn bps(value: u32) -> String {
    if value.is_multiple_of(100) {
        format!("{}%", value / 100)
    } else {
        let percent = f64::from(value) / 100.0;
        let text = format!("{percent:.2}");
        format!("{}%", text.trim_end_matches('0'))
    }
}

/// A hash or an address, shortened to `0x` and the first 4 … last 4, with an
/// ellipsis (U+2026). Anything that isn't long enough to shorten is shown
/// whole. A `sha256:` prefix is kept.
pub fn short_hash(value: &str) -> String {
    if let Some(digest) = value.strip_prefix("sha256:") {
        return format!("sha256:{}", elide(digest, 4, 4));
    }
    match value.strip_prefix("0x") {
        Some(hex) => format!("0x{}", elide(hex, 4, 4)),
        None => elide(value, 4, 4),
    }
}

fn elide(value: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= head + tail + 1 {
        return value.to_string();
    }
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{start}\u{2026}{end}")
}

fn month(month: Month) -> &'static str {
    match month {
        Month::January => "Jan",
        Month::February => "Feb",
        Month::March => "Mar",
        Month::April => "Apr",
        Month::May => "May",
        Month::June => "Jun",
        Month::July => "Jul",
        Month::August => "Aug",
        Month::September => "Sep",
        Month::October => "Oct",
        Month::November => "Nov",
        Month::December => "Dec",
    }
}

/// The full name of a month: August.
pub fn month_name(month: Month) -> &'static str {
    match month {
        Month::January => "January",
        Month::February => "February",
        Month::March => "March",
        Month::April => "April",
        Month::May => "May",
        Month::June => "June",
        Month::July => "July",
        Month::August => "August",
        Month::September => "September",
        Month::October => "October",
        Month::November => "November",
        Month::December => "December",
    }
}

/// The weekday, in full: Saturday.
pub fn weekday(date: Date) -> &'static str {
    match date.weekday() {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// A date: Sep 29, and Sep 29, 2025 when it isn't in `this_year`.
pub fn day(date: Date, this_year: i32) -> String {
    if date.year() == this_year {
        format!("{} {}", month(date.month()), date.day())
    } else {
        format!("{} {}, {}", month(date.month()), date.day(), date.year())
    }
}

/// A time of day in UTC, to the second: 14:32:40 UTC.
pub fn time(at: PrimitiveDateTime) -> String {
    format!("{:02}:{:02}:{:02} UTC", at.hour(), at.minute(), at.second())
}

/// A time of day in UTC, to the minute: 09:14 UTC.
pub fn time_short(at: PrimitiveDateTime) -> String {
    format!("{:02}:{:02} UTC", at.hour(), at.minute())
}

/// A date with its time: Sep 29, 09:14 UTC.
pub fn day_time(at: PrimitiveDateTime, this_year: i32) -> String {
    format!("{}, {}", day(at.date(), this_year), time_short(at))
}

/// A whole number of days: 1 day, 91 days.
pub fn days(count: u32) -> String {
    if count == 1 {
        "1 day".to_string()
    } else {
        format!("{} days", number(f64::from(count), 0))
    }
}

/// A runway in days, rounded down, as an estimate: About 41 days, About a
/// day, Under a day.
pub fn runway(days_left: f64) -> String {
    if !days_left.is_finite() {
        return "No charges yet".to_string();
    }
    let whole = days_left.floor();
    if whole < 1.0 {
        "Under a day".to_string()
    } else if whole < 2.0 {
        "About a day".to_string()
    } else {
        format!("About {} days", number(whole, 0))
    }
}

/// How long ago, said plainly and rounded down: 40 seconds, 3 minutes,
/// 2 hours, 4 days.
pub fn ago(seconds: f64) -> String {
    let seconds = seconds.max(0.0).floor() as u64;
    let (count, unit) = if seconds < 60 {
        (seconds, "second")
    } else if seconds < 3_600 {
        (seconds / 60, "minute")
    } else if seconds < 86_400 {
        (seconds / 3_600, "hour")
    } else {
        (seconds / 86_400, "day")
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn money_has_separators_cents_on_what_people_move_and_whole_dollars_for_totals() {
        assert_eq!(money(10_001.25), "$10,001.25");
        assert_eq!(money(0.5), "$0.50");
        assert_eq!(money_whole(2_418_330.4), "$2,418,330");
        assert_eq!(money_whole(999.5), "$1,000");
        assert_eq!(money_whole(0.0), "$0");
        assert_eq!(money(-12.5), "\u{2212}$12.50");
    }

    #[test]
    fn money_is_never_abbreviated() {
        assert_eq!(money_whole(2_400_000.0), "$2,400,000");
        assert!(!money_whole(1e9).contains('B'));
    }

    #[test]
    fn a_share_price_has_four_decimals() {
        assert_eq!(price(1.1402), "$1.1402");
        assert_eq!(price(1.0), "$1.0000");
        assert_eq!(price(1745.785789), "$1,745.7858");
    }

    #[test]
    fn a_return_is_signed_with_a_true_minus_and_one_decimal() {
        assert_eq!(ret(0.032), "+3.2%");
        assert_eq!(ret(-0.069), "\u{2212}6.9%");
        assert_eq!(ret(0.14), "+14.0%");
        assert_eq!(ret(1.5), "+150.0%");
        assert!(!ret(-0.05).contains('-'), "a hyphen is not a minus");
    }

    #[test]
    fn a_return_that_rounds_to_zero_has_no_sign_and_no_colour() {
        assert_eq!(ret(0.0), "0.0%");
        assert_eq!(ret(0.0004), "0.0%");
        assert_eq!(ret(-0.0004), "0.0%");
        assert_eq!(direction(-0.0004), Direction::Flat);
        assert_eq!(direction(0.0006), Direction::Up);
        assert_eq!(direction(-0.0006), Direction::Down);
    }

    #[test]
    fn shares_have_one_decimal_and_the_word() {
        assert_eq!(shares(8_771.44), "8,771.4 shares");
    }

    #[test]
    fn fees_read_as_the_docs_write_them() {
        assert_eq!(bps(10), "0.1%");
        assert_eq!(bps(100), "1%");
        assert_eq!(bps(1_500), "15%");
        assert_eq!(bps(25), "0.25%");
    }

    #[test]
    fn a_hash_shows_its_first_and_last_four() {
        assert_eq!(
            short_hash("0x4e1d30877432d1026706d7e805da846a32c3bb81e3c29b62179273c8eb5ba90b"),
            "0x4e1d\u{2026}a90b"
        );
        assert_eq!(
            short_hash("0x2d19b682575ec87a171ac826a6fce48478dcb4e8"),
            "0x2d19\u{2026}b4e8"
        );
        assert_eq!(
            short_hash("sha256:2961d4010c01acc1"),
            "sha256:2961\u{2026}acc1"
        );
        assert_eq!(short_hash("0xabc"), "0xabc");
        assert_eq!(
            short_hash("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"),
            "5Q54\u{2026}e4j1"
        );
    }

    #[test]
    fn times_are_utc_and_twenty_four_hour() {
        let at = datetime!(2026-09-29 14:32:40);
        assert_eq!(time(at), "14:32:40 UTC");
        assert_eq!(time_short(datetime!(2026-09-29 09:04:00)), "09:04 UTC");
        assert_eq!(day_time(at, 2026), "Sep 29, 14:32 UTC");
    }

    #[test]
    fn a_date_names_its_year_only_when_it_is_not_this_year() {
        let date = datetime!(2026-09-29 0:00).date();
        assert_eq!(day(date, 2026), "Sep 29");
        assert_eq!(day(date, 2027), "Sep 29, 2026");
        assert_eq!(weekday(date), "Tuesday");
    }

    #[test]
    fn durations_are_plain_words_rounded_down() {
        assert_eq!(days(1), "1 day");
        assert_eq!(days(91), "91 days");
        assert_eq!(runway(41.9), "About 41 days");
        assert_eq!(runway(1.2), "About a day");
        assert_eq!(runway(0.4), "Under a day");
        assert_eq!(ago(59.9), "59 seconds");
        assert_eq!(ago(180.0), "3 minutes");
        assert_eq!(ago(3_600.0), "1 hour");
    }
}
