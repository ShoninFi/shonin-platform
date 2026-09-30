//! Moving numbers across the boundary between the chain and a strategy.
//!
//! The chain counts in whole units: dollars to six places, sizes to eighteen.
//! A strategy thinks in decimals. Converting through text rather than through
//! a float means the number a strategy reads is exactly the number the chain
//! holds, and the number the chain receives is exactly what the strategy
//! meant, truncated towards zero so a rounding error can never grow a
//! position.

use alloy::primitives::{I256, U256};
use anyhow::{Context, Result};
use strategy_sdk::Decimal;

/// Dollars, as the asset counts them.
pub const ASSET_DECIMALS: u32 = 6;
/// Sizes, as the venue counts them.
pub const SIZE_DECIMALS: u32 = 18;

/// A whole-unit amount as a decimal.
pub fn to_decimal(value: U256, decimals: u32) -> Result<Decimal> {
    text(&value.to_string(), false, decimals)
        .parse()
        .context("a decimal from the chain")
}

/// The same, for a number that can be negative.
pub fn signed_to_decimal(value: I256, decimals: u32) -> Result<Decimal> {
    let negative = value.is_negative();
    let magnitude = if negative {
        (-value).into_raw().to_string()
    } else {
        value.into_raw().to_string()
    };
    text(&magnitude, negative, decimals)
        .parse()
        .context("a decimal from the chain")
}

/// A decimal as whole units, truncated towards zero.
pub fn to_units(value: Decimal, decimals: u32) -> Result<U256> {
    let (digits, negative) = units_text(value, decimals);
    if negative {
        return Ok(U256::ZERO);
    }
    digits.parse::<U256>().context("an amount for the chain")
}

/// The same, keeping the sign.
#[cfg(test)]
pub fn to_signed_units(value: Decimal, decimals: u32) -> Result<I256> {
    let (digits, negative) = units_text(value, decimals);
    let magnitude: I256 = digits.parse().context("an amount for the chain")?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Put the decimal point in: `1234567` with six places is `1.234567`.
fn text(digits: &str, negative: bool, decimals: u32) -> String {
    let places = decimals as usize;
    let sign = if negative { "-" } else { "" };
    if places == 0 {
        return format!("{sign}{digits}");
    }
    if digits.len() <= places {
        let padding = "0".repeat(places - digits.len());
        return format!("{sign}0.{padding}{digits}");
    }
    let (whole, fraction) = digits.split_at(digits.len() - places);
    format!("{sign}{whole}.{fraction}")
}

/// Take the decimal point out, dropping anything past the last place.
fn units_text(value: Decimal, decimals: u32) -> (String, bool) {
    let rendered = format!("{value}");
    let (negative, rest) = match rendered.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, rendered),
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole.to_string(), fraction.to_string()),
        None => (rest, String::new()),
    };
    let places = decimals as usize;
    let mut fraction = fraction;
    if fraction.len() < places {
        fraction.push_str(&"0".repeat(places - fraction.len()));
    } else {
        fraction.truncate(places);
    }
    let digits = format!("{whole}{fraction}");
    let trimmed = digits.trim_start_matches('0').to_string();
    (
        if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed
        },
        negative,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use strategy_sdk::decimal;

    #[test]
    fn dollars_cross_and_come_back_the_same() {
        let on_chain = U256::from(60_000_123_456u64);
        let as_decimal = to_decimal(on_chain, ASSET_DECIMALS).expect("it converts");
        assert_eq!(as_decimal, decimal("60000.123456"));
        assert_eq!(
            to_units(as_decimal, ASSET_DECIMALS).expect("it converts back"),
            on_chain
        );
    }

    #[test]
    fn a_size_keeps_all_eighteen_places() {
        let on_chain = U256::from(1_500_000_000_000_000_000u64);
        let as_decimal = to_decimal(on_chain, SIZE_DECIMALS).expect("it converts");
        assert_eq!(as_decimal, decimal("1.5"));
        assert_eq!(
            to_units(as_decimal, SIZE_DECIMALS).expect("it converts back"),
            on_chain
        );
    }

    #[test]
    fn something_smaller_than_a_unit_is_still_exact() {
        let as_decimal = to_decimal(U256::from(1u64), ASSET_DECIMALS).expect("it converts");
        assert_eq!(as_decimal, decimal("0.000001"));
    }

    #[test]
    fn a_short_position_keeps_its_sign() {
        let on_chain = I256::try_from(-2_000_000_000_000_000_000i64).expect("it fits");
        let as_decimal = signed_to_decimal(on_chain, SIZE_DECIMALS).expect("it converts");
        assert_eq!(as_decimal, decimal("-2"));
        assert_eq!(
            to_signed_units(as_decimal, SIZE_DECIMALS).expect("it converts back"),
            on_chain
        );
    }

    #[test]
    fn more_places_than_the_chain_holds_are_dropped_rather_than_rounded_up() {
        let more = decimal("0.0000019");
        assert_eq!(
            to_units(more, ASSET_DECIMALS).expect("it converts"),
            U256::from(1u64)
        );
    }

    #[test]
    fn a_negative_amount_where_none_is_possible_becomes_nothing() {
        assert_eq!(
            to_units(decimal("-5"), ASSET_DECIMALS).expect("it converts"),
            U256::ZERO
        );
    }

    #[test]
    fn zero_survives_the_trip() {
        assert_eq!(
            to_decimal(U256::ZERO, ASSET_DECIMALS).expect("it converts"),
            decimal("0")
        );
        assert_eq!(
            to_units(decimal("0"), SIZE_DECIMALS).expect("it converts"),
            U256::ZERO
        );
    }
}
