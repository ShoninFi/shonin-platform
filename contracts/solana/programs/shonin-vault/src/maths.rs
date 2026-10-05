//! The share and fee maths, with 256-bit intermediates.
//!
//! Every product here is taken in 256 bits and divided once, rounding down
//! unless the name says up, as the EVM vault's `mulDiv` does; a result that
//! doesn't fit its type is an error, never a wrap.

use anchor_lang::prelude::*;
use shonin_common::math::U256;

use crate::error::VaultError;
use crate::state::{Fees, Timers, BPS, PRICE};

fn narrow(value: U256) -> Result<u128> {
    if value > U256::from(u128::MAX) {
        return err!(VaultError::Overflow);
    }
    Ok(value.as_u128())
}

/// `a * b / d`, rounded down.
pub fn mul_div(a: u128, b: u128, d: u128) -> Result<u128> {
    require!(d != 0, VaultError::Overflow);
    narrow(U256::from(a) * U256::from(b) / U256::from(d))
}

/// `a * b / d`, rounded up.
pub fn mul_div_up(a: u128, b: u128, d: u128) -> Result<u128> {
    require!(d != 0, VaultError::Overflow);
    let product = U256::from(a) * U256::from(b);
    let d = U256::from(d);
    let mut quotient = product / d;
    if !(product % d).is_zero() {
        quotient += U256::one();
    }
    narrow(quotient)
}

/// `a / b`, rounded up.
pub fn div_up(a: u128, b: u128) -> Result<u128> {
    require!(b != 0, VaultError::Overflow);
    Ok(a.div_ceil(b))
}

/// `a * b / d`, rounded down, if it is no more than `max`.
pub fn mul_div_within(a: u128, b: u128, d: u128, max: u128) -> Result<Option<u128>> {
    require!(d != 0, VaultError::Overflow);
    let quotient = U256::from(a) * U256::from(b) / U256::from(d);
    Ok((quotient <= U256::from(max)).then(|| quotient.as_u128()))
}

/// `a * b / d`, rounded up, if it is no more than `max`.
pub fn mul_div_up_within(a: u128, b: u128, d: u128, max: u128) -> Result<Option<u128>> {
    require!(d != 0, VaultError::Overflow);
    let product = U256::from(a) * U256::from(b);
    let d = U256::from(d);
    let mut quotient = product / d;
    if !(product % d).is_zero() {
        quotient += U256::one();
    }
    Ok((quotient <= U256::from(max)).then(|| quotient.as_u128()))
}

/// `(a * wa + b * wb) / (wa + wb)`, rounded up: a mark averaged by shares.
pub fn weighted_up(a: u128, wa: u128, b: u128, wb: u128) -> Result<u128> {
    let total = U256::from(a) * U256::from(wa) + U256::from(b) * U256::from(wb);
    let weight = U256::from(wa) + U256::from(wb);
    require!(!weight.is_zero(), VaultError::Overflow);
    let mut quotient = total / weight;
    if !(total % weight).is_zero() {
        quotient += U256::one();
    }
    narrow(quotient)
}

/// A dollar amount, which must fit the token's 64 bits.
pub fn dollars(value: u128) -> Result<u64> {
    u64::try_from(value).map_err(|_| error!(VaultError::Overflow))
}

/// The profit fee owed on `gross` with `supply` shares at high-water mark
/// `mark`: a share of what the vault is worth above the mark, never below
/// zero. A basis at or past `gross` owes nothing however far past it is, so
/// it is never counted beyond that, and no mark or supply can overflow it.
pub fn fee_of(mark: u128, fees: &Fees, gross: u128, supply: u128) -> Result<u128> {
    let basis = match mul_div_up_within(mark, supply, PRICE, gross)? {
        Some(basis) if basis < gross => basis,
        _ => return Ok(0),
    };
    mul_div(
        gross - basis,
        fees.author_bps as u128 + fees.platform_bps as u128,
        BPS,
    )
}

/// The first slot boundary at least `slot_lead` from `now`: the earliest a
/// request made now can be priced.
pub fn next_slot(timers: &Timers, now: u64) -> Result<u64> {
    let interval = timers.slot_interval;
    let t = now
        .checked_add(timers.slot_lead)
        .ok_or(VaultError::Overflow)?;
    t.div_ceil(interval)
        .checked_mul(interval)
        .ok_or_else(|| error!(VaultError::Overflow))
}

/// Monday 00:00 UTC of the week after the one `t` falls in.
pub fn next_monday(t: u64) -> u64 {
    const DAY: u64 = 86_400;
    let day = t / DAY;
    // The first of January 1970 was a Thursday, three days after a Monday.
    let since_monday = day + 3;
    (since_monday - since_monday % 7 + 4) * DAY
}

/// A 256-bit number kept little-endian in an account.
pub fn wide(bytes: &[u8; 32]) -> U256 {
    U256::from_little_endian(bytes)
}

pub fn store_wide(value: U256) -> [u8; 32] {
    value.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_goes_the_way_each_name_says() {
        assert_eq!(mul_div(7, 3, 2).unwrap(), 10);
        assert_eq!(mul_div_up(7, 3, 2).unwrap(), 11);
        assert_eq!(mul_div_up(6, 3, 2).unwrap(), 9);
        assert!(mul_div(u128::MAX, u128::MAX, 1).is_err());
        assert_eq!(mul_div(u128::MAX, u128::MAX, u128::MAX).unwrap(), u128::MAX);
        assert_eq!(mul_div_within(7, 3, 2, 10).unwrap(), Some(10));
        assert_eq!(mul_div_within(7, 3, 2, 9).unwrap(), None);
        assert_eq!(
            mul_div_within(u128::MAX, u128::MAX, 1, u128::MAX).unwrap(),
            None
        );
        assert_eq!(mul_div_up_within(7, 3, 2, 11).unwrap(), Some(11));
        assert_eq!(mul_div_up_within(7, 3, 2, 10).unwrap(), None);
        assert_eq!(
            mul_div_up_within(u128::MAX, u128::MAX, 1, u128::MAX).unwrap(),
            None
        );
    }

    #[test]
    fn a_basis_too_large_to_count_owes_no_fee() {
        let fees = Fees {
            author_bps: 1_000,
            platform_bps: 1_000,
            ..Fees::default()
        };
        assert_eq!(fee_of(u128::MAX, &fees, 1_000, u128::MAX).unwrap(), 0);
        assert_eq!(fee_of(PRICE, &fees, 1_000, 1_000).unwrap(), 0);
        assert_eq!(fee_of(PRICE, &fees, 2_000, 1_000).unwrap(), 200);
    }

    #[test]
    fn the_next_monday_follows_the_week_a_time_falls_in() {
        // 5 January 1970 was a Monday.
        let monday = 4 * 86_400;
        assert_eq!(next_monday(0), monday);
        assert_eq!(next_monday(monday - 1), monday);
        assert_eq!(next_monday(monday), monday + 7 * 86_400);
    }

    #[test]
    fn a_request_is_priced_at_the_first_boundary_its_lead_reaches() {
        let timers = Timers {
            slot_interval: 10,
            slot_lead: 5,
            ..Timers::default()
        };
        assert_eq!(next_slot(&timers, 3).unwrap(), 10);
        assert_eq!(next_slot(&timers, 5).unwrap(), 10);
        assert_eq!(next_slot(&timers, 6).unwrap(), 20);
    }
}
