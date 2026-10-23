//! Amounts as the chain counts them: whole numbers of a token's smallest
//! unit. Text a person types becomes base units here, exactly, or is refused
//! with a reason; nothing that is sent or compared ever passes through a
//! float.

use alloy_primitives::{U256, U512};

/// Why typed text isn't an amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AmountError {
    /// Nothing typed.
    Empty,
    /// Not a plain decimal number: letters, a second point, a sign, a comma.
    NotANumber,
    /// More digits after the point than the token has.
    TooManyDecimals { allowed: u8 },
    /// Bigger than anything the chain can hold.
    TooLarge,
}

impl AmountError {
    /// What to tell the person, under the field.
    pub fn words(&self) -> String {
        match self {
            Self::Empty => "Type an amount.".to_string(),
            Self::NotANumber => "Use digits and a point, like 1000.50.".to_string(),
            Self::TooManyDecimals { allowed } => {
                format!("Use at most {allowed} digits after the point.")
            }
            Self::TooLarge => "That number is too large.".to_string(),
        }
    }
}

/// Typed text as base units of a token with `decimals` decimals.
///
/// Takes digits with at most one point (`1000`, `1000.5`, `.5`, `5.`), and a
/// leading `$` or surrounding spaces. Refuses commas outright, rather than
/// guess whether `1,500` means fifteen hundred or one and a half; refuses
/// signs, exponents and anything else; and refuses more decimal digits than
/// the token has rather than silently dropping them.
pub fn parse_units(text: &str, decimals: u8) -> Result<U256, AmountError> {
    let text = text.trim();
    let text = text.strip_prefix('$').unwrap_or(text).trim_start();
    if text.is_empty() {
        return Err(AmountError::Empty);
    }
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (text, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return Err(AmountError::NotANumber);
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountError::NotANumber);
    }
    // Trailing zeros after the point carry nothing: 1.500000000 is 1.5.
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > usize::from(decimals) {
        return Err(AmountError::TooManyDecimals { allowed: decimals });
    }
    let mut digits = String::with_capacity(whole.len() + usize::from(decimals));
    digits.push_str(whole);
    digits.push_str(fraction);
    for _ in fraction.len()..usize::from(decimals) {
        digits.push('0');
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(digits, 10).map_err(|_| AmountError::TooLarge)
}

/// Which way a figure shown with fewer digits than it has is rounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    /// Toward zero: what someone gets is never overstated.
    Down,
    /// Away from zero: what someone pays is never understated.
    Up,
    /// To the nearest, a half away from zero: an estimate shown with "≈"
    /// or "about", and never a figure that bounds what is sent.
    Nearest,
}

/// Base units as decimal text with exactly `places` digits after the point
/// (none when `places` is zero), grouped in threes: 1234567890 with six
/// decimals and two places is `1,234.56` rounded down, `1,234.57` up.
pub fn format_units(value: U256, decimals: u8, places: u8, rounding: Rounding) -> String {
    let (whole, fraction) = split(value, decimals, places, rounding);
    let whole = grouped(&whole.to_string());
    if places == 0 {
        whole
    } else {
        format!("{whole}.{fraction:0>width$}", width = usize::from(places))
    }
}

/// The same with every digit the value has, but never fewer than `places`
/// after the point (nor more than the token has): an amount someone sends is
/// shown exactly. 1000500000
/// with six decimals is `1,000.50`; 1000500001 is `1,000.500001`.
pub fn format_exact(value: U256, decimals: u8, places: u8) -> String {
    let places = usize::from(places.min(decimals));
    let scale = pow10(decimals);
    let whole = value / scale;
    let fraction = value % scale;
    let mut fraction = if decimals == 0 {
        String::new()
    } else {
        format!("{fraction:0>width$}", width = usize::from(decimals))
    };
    while fraction.len() > places && fraction.ends_with('0') {
        fraction.pop();
    }
    let whole = grouped(&whole.to_string());
    if fraction.is_empty() {
        whole
    } else {
        format!("{whole}.{fraction}")
    }
}

/// The value as `format_units` shows it, back in base units: 1234567 with
/// six decimals to two places is 1230000 rounded down, 1240000 up. Figures
/// shown side by side can then be added and taken away as read.
pub fn rounded(value: U256, decimals: u8, places: u8, rounding: Rounding) -> U256 {
    let places = places.min(decimals);
    let (whole, fraction) = split(value, decimals, places, rounding);
    whole
        .saturating_mul(pow10(places))
        .saturating_add(fraction)
        .saturating_mul(pow10(decimals - places))
}

/// The whole part and `places` digits of fraction, rounded as asked.
fn split(value: U256, decimals: u8, places: u8, rounding: Rounding) -> (U256, U256) {
    let places = places.min(decimals);
    // The value in units of the last digit shown.
    let step = pow10(decimals - places);
    let mut shown = value / step;
    let rest = value % step;
    let up = match rounding {
        Rounding::Down => false,
        Rounding::Up => !rest.is_zero(),
        // A half or more of the last digit shown.
        Rounding::Nearest => rest.saturating_mul(U256::from(2u8)) >= step,
    };
    if up {
        shown = shown.saturating_add(U256::from(1u8));
    }
    let scale = pow10(places);
    (shown / scale, shown % scale)
}

/// Ten to the power `exponent`, for exponents a token's decimals can take
/// (a `u8`, so at most 255; anything past 77 doesn't fit and saturates).
pub fn pow10(exponent: u8) -> U256 {
    U256::from(10u8)
        .checked_pow(U256::from(exponent))
        .unwrap_or(U256::MAX)
}

/// Group digits in threes: 2418330 becomes 2,418,330.
pub fn grouped(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `a * b / c`, rounded down, as Solidity's `mulDiv` does it: the product is
/// taken at full width, so it never overflows on the way. `None` when `c` is
/// zero or the result doesn't fit.
pub fn mul_div(a: U256, b: U256, c: U256) -> Option<U256> {
    if c.is_zero() {
        return None;
    }
    let product: U512 = a.widening_mul(b);
    narrow(product / widen(c))
}

/// The same, rounded up (`Math.Rounding.Ceil`).
pub fn mul_div_up(a: U256, b: U256, c: U256) -> Option<U256> {
    if c.is_zero() {
        return None;
    }
    let product: U512 = a.widening_mul(b);
    let c = widen(c);
    let quotient = product / c;
    let quotient = if (product % c).is_zero() {
        quotient
    } else {
        quotient + U512::from(1u8)
    };
    narrow(quotient)
}

fn widen(value: U256) -> U512 {
    let mut limbs = [0u64; 8];
    limbs[..4].copy_from_slice(value.as_limbs());
    U512::from_limbs(limbs)
}

fn narrow(value: U512) -> Option<U256> {
    let limbs = value.as_limbs();
    if limbs[4..].iter().any(|limb| *limb != 0) {
        return None;
    }
    Some(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> U256 {
        parse_units(text, 6).unwrap()
    }

    #[test]
    fn typed_dollars_become_exact_base_units() {
        assert_eq!(units("1000"), U256::from(1_000_000_000u64));
        assert_eq!(units("1000.5"), U256::from(1_000_500_000u64));
        assert_eq!(units("0.000001"), U256::from(1u8));
        assert_eq!(units(".5"), U256::from(500_000u64));
        assert_eq!(units("5."), U256::from(5_000_000u64));
        assert_eq!(units(" $10 "), U256::from(10_000_000u64));
        assert_eq!(units("$ 10"), U256::from(10_000_000u64));
        assert_eq!(units("0"), U256::ZERO);
        assert_eq!(units("000.000"), U256::ZERO);
        assert_eq!(units("007.10"), U256::from(7_100_000u64));
    }

    #[test]
    fn a_decimal_that_no_float_can_hold_is_still_exact() {
        // 0.1 + 0.2 is not 0.3 in binary; in base units it is.
        let sum = units("0.1") + units("0.2");
        assert_eq!(sum, units("0.3"));
        // A value past 2^53 base units keeps its last digit.
        assert_eq!(
            units("9007199254.740993"),
            U256::from(9_007_199_254_740_993u64)
        );
    }

    #[test]
    fn a_rounded_value_is_the_figure_shown_in_base_units() {
        let value = U256::from(1_234_567u64);
        assert_eq!(
            rounded(value, 6, 2, Rounding::Down),
            U256::from(1_230_000u64)
        );
        assert_eq!(rounded(value, 6, 2, Rounding::Up), U256::from(1_240_000u64));
        assert_eq!(
            rounded(value, 6, 2, Rounding::Nearest),
            U256::from(1_230_000u64)
        );
        assert_eq!(
            rounded(U256::from(1_995_000u64), 6, 2, Rounding::Nearest),
            U256::from(2_000_000u64)
        );
        // A token with fewer decimals than places is already exact.
        assert_eq!(
            rounded(U256::from(7u8), 1, 2, Rounding::Up),
            U256::from(7u8)
        );
    }

    #[test]
    fn trailing_zeros_past_the_tokens_decimals_are_not_extra_digits() {
        assert_eq!(units("1.500000000000"), U256::from(1_500_000u64));
    }

    #[test]
    fn more_digits_than_the_token_has_are_refused_not_dropped() {
        assert_eq!(
            parse_units("1.0000001", 6),
            Err(AmountError::TooManyDecimals { allowed: 6 })
        );
        assert_eq!(
            parse_units("0.1", 0),
            Err(AmountError::TooManyDecimals { allowed: 0 })
        );
    }

    #[test]
    fn anything_but_a_plain_decimal_is_refused() {
        for text in [
            "1,500", "1.5.0", "-5", "+5", "1e6", "0x10", "1_000", "abc", ".", "$", "1 000", "١٢",
            "5$", "NaN", "inf",
        ] {
            assert!(parse_units(text, 6).is_err(), "{text:?} was taken");
        }
        assert_eq!(parse_units("", 6), Err(AmountError::Empty));
        assert_eq!(parse_units("   ", 6), Err(AmountError::Empty));
    }

    #[test]
    fn the_largest_amount_the_chain_holds_parses_and_one_more_is_refused() {
        let max = U256::MAX.to_string();
        assert_eq!(parse_units(&max, 0), Ok(U256::MAX));
        let over = "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        assert_eq!(parse_units(over, 0), Err(AmountError::TooLarge));
        // With decimals, the whole part overflows sooner.
        assert_eq!(parse_units(&max, 6), Err(AmountError::TooLarge));
    }

    #[test]
    fn eighteen_decimal_shares_parse_exactly() {
        assert_eq!(
            parse_units("8771.4", 18).unwrap(),
            U256::from(8_771_400_000_000_000_000_000u128)
        );
        assert_eq!(
            parse_units("0.000000000000000001", 18).unwrap(),
            U256::from(1u8)
        );
    }

    #[test]
    fn figures_are_grouped_and_rounded_the_way_asked() {
        let value = U256::from(1_234_567_890u64);
        assert_eq!(format_units(value, 6, 2, Rounding::Down), "1,234.56");
        assert_eq!(format_units(value, 6, 2, Rounding::Up), "1,234.57");
        assert_eq!(format_units(value, 6, 0, Rounding::Down), "1,234");
        assert_eq!(format_units(value, 6, 0, Rounding::Up), "1,235");
        assert_eq!(format_units(value, 6, 6, Rounding::Down), "1,234.567890");
        assert_eq!(format_units(U256::ZERO, 6, 2, Rounding::Up), "0.00");
        assert_eq!(format_units(U256::from(1u8), 6, 2, Rounding::Down), "0.00");
        assert_eq!(format_units(U256::from(1u8), 6, 2, Rounding::Up), "0.01");
        // A whole number never grows a fraction it didn't have.
        assert_eq!(
            format_units(U256::from(5_000_000u64), 6, 2, Rounding::Up),
            "5.00"
        );
    }

    #[test]
    fn an_amount_sent_is_shown_with_every_digit_it_has() {
        assert_eq!(format_exact(U256::from(1_000_500_000u64), 6, 2), "1,000.50");
        assert_eq!(
            format_exact(U256::from(1_000_500_001u64), 6, 2),
            "1,000.500001"
        );
        assert_eq!(format_exact(U256::from(1u8), 6, 2), "0.000001");
        assert_eq!(format_exact(U256::ZERO, 6, 2), "0.00");
        assert_eq!(format_exact(U256::from(42u8), 0, 2), "42");
    }

    #[test]
    fn the_largest_value_formats_without_overflow() {
        let text = format_units(U256::MAX, 18, 1, Rounding::Up);
        assert!(text.starts_with(
            "115,792,089,237,316,195,423,570,985,008,687,907,853,269,984,665,640,564,039,457"
        ));
        assert_eq!(
            format_units(U256::MAX, 0, 0, Rounding::Down),
            grouped(&U256::MAX.to_string())
        );
    }

    #[test]
    fn mul_div_rounds_as_the_contracts_do_and_never_overflows_midway() {
        let seven = U256::from(7u8);
        let two = U256::from(2u8);
        assert_eq!(mul_div(seven, U256::from(1u8), two), Some(U256::from(3u8)));
        assert_eq!(
            mul_div_up(seven, U256::from(1u8), two),
            Some(U256::from(4u8))
        );
        assert_eq!(
            mul_div_up(U256::from(8u8), U256::from(1u8), two),
            Some(U256::from(4u8))
        );
        // The product overflows 256 bits; the quotient doesn't.
        assert_eq!(mul_div(U256::MAX, U256::MAX, U256::MAX), Some(U256::MAX));
        assert_eq!(mul_div(U256::MAX, two, U256::from(1u8)), None);
        assert_eq!(mul_div(seven, seven, U256::ZERO), None);
        assert_eq!(mul_div_up(seven, seven, U256::ZERO), None);
    }
    #[test]
    fn an_estimate_rounds_to_the_nearest_while_paid_and_received_figures_keep_their_direction() {
        // A payout of 9.998497 reads as about 10.00, not 9.99.
        let payout = units("9.998497");
        assert_eq!(format_units(payout, 6, 2, Rounding::Nearest), "10.00");
        assert_eq!(format_units(payout, 6, 2, Rounding::Down), "9.99");
        assert_eq!(format_units(payout, 6, 2, Rounding::Up), "10.00");
        // A half goes up; under a half goes down.
        assert_eq!(
            format_units(units("49.945"), 6, 2, Rounding::Nearest),
            "49.95"
        );
        assert_eq!(
            format_units(units("49.944999"), 6, 2, Rounding::Nearest),
            "49.94"
        );
        assert_eq!(format_units(units("2.5"), 6, 0, Rounding::Nearest), "3");
        assert_eq!(
            format_units(units("1234.5"), 6, 0, Rounding::Nearest),
            "1,235"
        );
        // A cost stays never understated, and what's received never
        // overstated.
        assert_eq!(format_units(units("0.001"), 6, 2, Rounding::Up), "0.01");
        assert_eq!(
            format_units(units("0.001"), 6, 2, Rounding::Nearest),
            "0.00"
        );
        assert_eq!(format_units(units("0.009"), 6, 2, Rounding::Down), "0.00");
        // Exact figures are untouched.
        assert_eq!(
            format_units(units("12.34"), 6, 2, Rounding::Nearest),
            "12.34"
        );
    }
}
