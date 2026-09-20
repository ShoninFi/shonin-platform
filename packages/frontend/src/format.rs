//! How numbers, hashes and dates are written throughout the app.
//!
//! One place, so a figure reads the same on a leaderboard row, on a record
//! page and in the pre-rendered HTML. No web dependencies: the build-time
//! pre-render uses exactly these functions.

use time::{Date, Duration, Month, PrimitiveDateTime};

/// Group the integer part in threes: 1204331 becomes 1,204,331.
fn grouped(whole: i64) -> String {
    let digits = whole.abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    if whole < 0 { format!("-{out}") } else { out }
}

/// A number with thousands separators and a fixed number of decimals.
pub fn number(value: f64, decimals: usize) -> String {
    let rounded = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = rounded.split_once('.').unwrap_or((rounded.as_str(), ""));
    let whole: i64 = whole.parse().unwrap_or(0);
    let sign = if value < 0.0 { "-" } else { "" };
    if fraction.is_empty() {
        format!("{sign}{}", grouped(whole))
    } else {
        format!("{sign}{}.{fraction}", grouped(whole))
    }
}

/// Money, to the dollar: $1,204,331. A negative amount carries its sign
/// before the currency, never inside it.
pub fn money(value: f64) -> String {
    signed_money(value, 0)
}

/// Money, to the cent, for amounts a person is about to move.
pub fn money_exact(value: f64) -> String {
    signed_money(value, 2)
}

fn signed_money(value: f64, decimals: usize) -> String {
    if value < 0.0 {
        format!("−${}", number(-value, decimals))
    } else {
        format!("${}", number(value, decimals))
    }
}

/// Money at a glance: $3.24M, $412k, $86.40.
pub fn money_compact(value: f64) -> String {
    let size = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if size >= 1_000_000_000.0 {
        format!("{sign}${:.2}B", size / 1_000_000_000.0)
    } else if size >= 1_000_000.0 {
        format!("{sign}${:.2}M", size / 1_000_000.0)
    } else if size >= 1_000.0 {
        format!("{sign}${:.0}k", size / 1_000.0)
    } else if size >= 1.0 {
        format!("{sign}${size:.0}")
    } else {
        format!("{sign}${size:.2}")
    }
}

/// A fraction as a percentage, with a sign: +18.4%, −4.1%.
pub fn percent_signed(fraction: f64, decimals: usize) -> String {
    let value = fraction * 100.0;
    if value > 0.0 {
        format!("+{:.*}%", decimals, value)
    } else if value < 0.0 {
        // A proper minus sign: it aligns with digits in tabular figures.
        format!("−{:.*}%", decimals, value.abs())
    } else {
        format!("{:.*}%", decimals, 0.0)
    }
}

/// A fraction as a percentage, unsigned: 18.4%.
pub fn percent(fraction: f64, decimals: usize) -> String {
    format!("{:.*}%", decimals, fraction * 100.0)
}

/// An annualised return, as the leaderboard writes it.
pub fn annualised(fraction: f64) -> String {
    format!("{} ann.", percent_signed(fraction, 1))
}

/// Basis points as a percentage: 50 becomes 0.5%.
pub fn from_bps(bps: u32) -> String {
    let value = f64::from(bps) / 100.0;
    if (value - value.round()).abs() < 0.001 {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

/// A vault's value per share, which is always read to four decimals.
pub fn share_price(value: f64) -> String {
    format!("{value:.4}")
}

/// A whole number of things: 3,902 trades.
pub fn count(value: u32) -> String {
    grouped(i64::from(value))
}

/// The middle of a hash or an address, elided: sha256:9c41a8…b2e1.
pub fn short_hash(hash: &str) -> String {
    match hash.split_once(':') {
        Some((prefix, digest)) => format!("{prefix}:{}", elide(digest)),
        None => elide(hash),
    }
}

/// An address, elided the same way: 0x7e3a91…4c8d.
pub fn short_address(address: &str) -> String {
    elide(address)
}

fn elide(value: &str) -> String {
    let body = value.strip_prefix("0x").unwrap_or(value);
    let prefix = if value.starts_with("0x") { "0x" } else { "" };
    if body.len() <= 12 {
        return value.to_string();
    }
    format!("{prefix}{}…{}", &body[..6], &body[body.len() - 4..])
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

/// 20 Sep 2026.
pub fn date(date: Date) -> String {
    format!("{} {} {}", date.day(), month(date.month()), date.year())
}

/// Sep 2026, for a chart axis or a monthly table.
pub fn month_year(date: Date) -> String {
    format!("{} {}", month(date.month()), date.year())
}

/// 20 Sep, for a dense axis.
pub fn day_month(date: Date) -> String {
    format!("{} {}", date.day(), month(date.month()))
}

/// 20 Sep 2026, 14:20 UTC.
pub fn timestamp(at: PrimitiveDateTime) -> String {
    format!(
        "{} {} {}, {:02}:{:02} UTC",
        at.day(),
        month(at.month()),
        at.year(),
        at.hour(),
        at.minute()
    )
}

/// How long ago something happened, read from a fixed "now". Past two days
/// it reads in days, because that is how people talk about the past.
pub fn ago(at: PrimitiveDateTime, now: PrimitiveDateTime) -> String {
    let span = now - at;
    if span.is_negative() {
        return "just now".to_string();
    }
    match span_words(span, 48) {
        None => "just now".to_string(),
        Some(words) => format!("{words} ago"),
    }
}

/// How long until something happens. It stays in hours for three days,
/// because the things the app counts down to — a blessing expiring, a
/// timelock running out, a vault freezing — are quoted in hours.
pub fn until(at: PrimitiveDateTime, now: PrimitiveDateTime) -> String {
    let span = at - now;
    if span.is_negative() {
        return "expired".to_string();
    }
    match span_words(span, 72) {
        None => "any moment".to_string(),
        Some(words) => format!("in {words}"),
    }
}

/// A span in the largest unit that still says something, or `None` when it
/// is too short to be worth a number.
fn span_words(span: Duration, hour_limit: i64) -> Option<String> {
    let minutes = span.whole_minutes();
    if minutes < 1 {
        None
    } else if minutes < 60 {
        Some(format!("{minutes}m"))
    } else if span.whole_hours() < hour_limit {
        Some(format!("{}h", span.whole_hours()))
    } else if span.whole_days() < 90 {
        Some(format!("{}d", span.whole_days()))
    } else {
        Some(format!("{}mo", span.whole_days() / 30))
    }
}

/// A run of days, as a record's age is written: 214d.
pub fn days(days: u32) -> String {
    if days >= 730 {
        format!("{}y", days / 365)
    } else {
        format!("{days}d")
    }
}

/// A build's duration: 2m 22s.
pub fn duration(seconds: u32) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

/// A file size: 184 MiB, 4.1 KiB.
pub fn bytes(value: u32) -> String {
    let size = f64::from(value);
    if size >= 1_048_576.0 {
        format!("{:.1} MiB", size / 1_048_576.0)
    } else if size >= 1024.0 {
        format!("{:.1} KiB", size / 1024.0)
    } else {
        format!("{value} B")
    }
}

/// A rate the tank is charged at: $4.80 a day.
pub fn per_day(value: f64) -> String {
    format!("{} a day", money_exact(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_grouped_and_rounded() {
        assert_eq!(money(1_204_331.42), "$1,204,331");
        assert_eq!(money_exact(86.4), "$86.40");
        assert_eq!(money(0.0), "$0");
        assert_eq!(money(-2_400.0), "−$2,400");
        assert_eq!(money_exact(-20.0), "−$20.00");
    }

    #[test]
    fn compact_money_picks_its_unit() {
        assert_eq!(money_compact(3_240_000.0), "$3.24M");
        assert_eq!(money_compact(412_000.0), "$412k");
        assert_eq!(money_compact(86.4), "$86");
        assert_eq!(money_compact(0.42), "$0.42");
        assert_eq!(money_compact(1_500_000_000.0), "$1.50B");
    }

    #[test]
    fn a_signed_percentage_always_carries_its_sign() {
        assert_eq!(percent_signed(0.184, 1), "+18.4%");
        assert_eq!(percent_signed(-0.041, 1), "−4.1%");
        assert_eq!(percent_signed(0.0, 1), "0.0%");
    }

    #[test]
    fn hashes_keep_their_prefix_when_elided() {
        assert_eq!(
            short_hash("sha256:9c41a8b7d6e5f40312ab"),
            "sha256:9c41a8…12ab"
        );
        assert_eq!(short_address("0x7e3a91cd5b2f84d0"), "0x7e3a91…84d0");
        assert_eq!(short_address("0x7e3a"), "0x7e3a");
    }

    #[test]
    fn dates_and_spans_read_as_words() {
        let day = Date::from_calendar_date(2026, Month::September, 20).unwrap();
        assert_eq!(date(day), "20 Sep 2026");
        assert_eq!(month_year(day), "Sep 2026");

        let now = day.with_hms(14, 20, 0).unwrap();
        assert_eq!(ago(now - Duration::hours(3), now), "3h ago");
        assert_eq!(ago(now - Duration::days(5), now), "5d ago");
        assert_eq!(ago(now - Duration::seconds(20), now), "just now");
        assert_eq!(until(now + Duration::hours(51), now), "in 51h");
        assert_eq!(until(now - Duration::hours(1), now), "expired");
    }

    #[test]
    fn small_units_read_plainly() {
        assert_eq!(days(214), "214d");
        assert_eq!(duration(142), "2m 22s");
        assert_eq!(duration(38), "38s");
        assert_eq!(from_bps(50), "0.5%");
        assert_eq!(from_bps(1000), "10%");
        assert_eq!(count(3902), "3,902");
        assert_eq!(share_price(1.08423), "1.0842");
    }
}
