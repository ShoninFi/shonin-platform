//! Money figures in the design system's words, straight from base units, so
//! a figure on a money screen is the chain's number rounded once, the way the
//! screen says: dollars with separators and cents, a share price to four
//! decimals, shares to one, a true minus. What someone gets is rounded down
//! and what they pay is shown exactly, so neither is ever overstated.

use alloy_primitives::U256;

use super::units::{Rounding, format_exact, format_units};

/// The true minus sign, which lines up with digits in tabular figures.
pub const MINUS: char = '\u{2212}';

/// Dollars, to the cent, rounded as asked: `$1,000.00`.
pub fn dollars(value: U256, decimals: u8, rounding: Rounding) -> String {
    format!("${}", format_units(value, decimals, 2, rounding))
}

/// Dollars someone pays or sends, with every digit they have and at least
/// the cents: `$1,000.00`, `$1,000.000001`.
pub fn dollars_exact(value: U256, decimals: u8) -> String {
    format!("${}", format_exact(value, decimals, 2))
}

/// A fee or other amount taken away, with a true minus: `−$2.28`.
pub fn minus_dollars(value: U256, decimals: u8, rounding: Rounding) -> String {
    if value.is_zero() {
        return dollars(value, decimals, rounding);
    }
    format!("{MINUS}{}", dollars(value, decimals, rounding))
}

/// A token amount with its symbol, exactly: `1,000.00 USDC`.
pub fn token_exact(value: U256, decimals: u8, symbol: &str) -> String {
    format!("{} {symbol}", format_exact(value, decimals, 2))
}

/// A wallet balance with its symbol, to the cent, rounded down so the field
/// never offers more than is there: `4,250.00 USDC`.
pub fn token_balance(value: U256, decimals: u8, symbol: &str) -> String {
    format!(
        "{} {symbol}",
        format_units(value, decimals, 2, Rounding::Down)
    )
}

/// The same balance as plain text a field takes back ("Use 812.40"): no
/// separators, the cents rounded down.
pub fn plain(value: U256, decimals: u8) -> String {
    format_units(value, decimals, 2.min(decimals), Rounding::Down).replace(',', "")
}

/// Every digit, no separators: for a "Max" that must send exactly what is
/// held, not a rounded figure.
pub fn plain_exact(value: U256, decimals: u8) -> String {
    format_exact(value, decimals, 0).replace(',', "")
}

/// A share price, to four decimals: `$1.1402`. Rounded to the nearest
/// ten-thousandth would hide which way it went, so it rounds down.
pub fn price(value: U256, decimals: u8) -> String {
    format!("${}", format_units(value, decimals, 4, Rounding::Down))
}

/// Shares, to one decimal, rounded as asked: `876.2`.
pub fn shares(value: U256, rounding: Rounding) -> String {
    format_units(value, 18, 1, rounding)
}

/// The fallback's units, a fraction of what the stations hold scaled by
/// 1e18, as a percentage to four decimals rounded down: `1.2345%`; a
/// fraction too small to show says so rather than read as none.
pub fn units_share(value: U256) -> String {
    let shown = format_units(value, 16, 4, Rounding::Down);
    if !value.is_zero() && shown == "0.0000" {
        return "less than 0.0001%".to_string();
    }
    format!("{shown}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(units: u64) -> U256 {
        U256::from(units)
    }

    #[test]
    fn dollars_read_as_the_design_writes_them() {
        assert_eq!(dollars(usd(1_000_000_000), 6, Rounding::Down), "$1,000.00");
        assert_eq!(dollars(usd(2_277_129_999), 6, Rounding::Down), "$2,277.12");
        assert_eq!(dollars(usd(2_277_120_001), 6, Rounding::Up), "$2,277.13");
        assert_eq!(dollars_exact(usd(10_000_000), 6), "$10.00");
        assert_eq!(dollars_exact(usd(10_000_001), 6), "$10.000001");
        assert_eq!(
            minus_dollars(usd(2_280_000), 6, Rounding::Up),
            "\u{2212}$2.28"
        );
        assert_eq!(minus_dollars(U256::ZERO, 6, Rounding::Up), "$0.00");
    }

    #[test]
    fn a_wallet_balance_is_never_rounded_up() {
        assert_eq!(token_balance(usd(812_409_999), 6, "USDC"), "812.40 USDC");
        assert_eq!(plain(usd(812_409_999), 6), "812.40");
        assert_eq!(plain(usd(4_250_000_000), 6), "4250.00");
        assert_eq!(plain_exact(usd(812_409_999), 6), "812.409999");
        assert_eq!(plain_exact(usd(812_000_000), 6), "812");
    }

    #[test]
    fn share_prices_have_four_decimals_and_shares_one() {
        assert_eq!(price(usd(1_140_200), 6), "$1.1402");
        assert_eq!(price(usd(1_140_299), 6), "$1.1402");
        let held = U256::from(8_771_449_999_999_999_999_999u128);
        assert_eq!(shares(held, Rounding::Down), "8,771.4");
        assert_eq!(shares(held, Rounding::Up), "8,771.5");
    }

    #[test]
    fn the_fallbacks_units_read_as_a_share_of_the_stations_never_as_raw_units() {
        // 1.23% of what the stations hold, scaled by 1e18.
        let units = U256::from(12_300_000_000_000_000u64);
        assert_eq!(units_share(units), "1.2300%");
        assert_eq!(units_share(U256::from(7u8)), "less than 0.0001%");
        assert_eq!(units_share(U256::ZERO), "0.0000%");
        assert!(!units_share(units).contains("12300000"));
    }
}
