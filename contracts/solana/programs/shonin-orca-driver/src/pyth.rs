//! Pyth's price of the asset, read from the one account the driver lists.
//!
//! The account must be exactly the listed address, owned by Pyth's receiver
//! program, a `PriceUpdateV2`, for the listed feed: anything else is not the
//! price at all, and the driver refuses it rather than answer. Whether the
//! price is fit to value with (fully verified, fresh, its confidence inside
//! the band) is a fact about the price, and a driver answers it with
//! `outside` instead; so is a price that can't be read as one at all
//! (nothing or less, or an exponent out of reach).

use anchor_lang::prelude::*;

use crate::network::{FEED_ID, PRICE_ACCOUNT, PYTH_RECEIVER};
use crate::DriverError;

/// `sha256("account:PriceUpdateV2")[..8]`.
pub const PRICE_UPDATE: [u8; 8] = [0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd];

/// A price as Pyth gives it: `price * 10^exponent` dollars a whole token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub price: u64,
    pub confidence: u64,
    pub exponent: i32,
    pub publish_time: i64,
    /// Whether every guardian signature was checked (`Full`), not only some.
    pub full: bool,
}

impl Price {
    /// Reads the listed price account: `None` when it is the price account
    /// but holds no price the driver can read.
    pub fn load(info: &AccountInfo) -> Result<Option<Self>> {
        require_keys_eq!(*info.key, PRICE_ACCOUNT, DriverError::WrongAccounts);
        require_keys_eq!(*info.owner, PYTH_RECEIVER, DriverError::WrongAccounts);
        let data = info.try_borrow_data()?;
        require!(
            data.len() >= 134 && data[..8] == PRICE_UPDATE,
            DriverError::WrongAccounts
        );
        // The verification level is one byte for `Full` (1) and two for
        // `Partial` (0, then how many signatures); what follows shifts with
        // it.
        let (full, at) = match data[40] {
            1 => (true, 41),
            0 => (false, 42),
            _ => return err!(DriverError::WrongAccounts),
        };
        require!(data.len() >= at + 68, DriverError::WrongAccounts);
        require!(data[at..at + 32] == FEED_ID, DriverError::WrongAccounts);
        let i64_at = |offset: usize| {
            i64::from_le_bytes(data[at + offset..at + offset + 8].try_into().unwrap())
        };
        let price = i64_at(32);
        let confidence = i64_at(40) as u64;
        let exponent = i32::from_le_bytes(data[at + 48..at + 52].try_into().unwrap());
        let publish_time = i64_at(52);
        // A price of nothing or less prices nothing.
        if price <= 0 || !(-18..=18).contains(&exponent) {
            return Ok(None);
        }
        Ok(Some(Self {
            price: price as u64,
            confidence,
            exponent,
            publish_time,
            full,
        }))
    }

    /// Whether the price is fit to value with at `now` inside `band_bps`:
    /// fully verified, no older than `max_age`, and its confidence no wider
    /// than the band.
    pub fn fit(&self, now: i64, max_age: u64, band_bps: u16) -> bool {
        let age = now.saturating_sub(self.publish_time).max(0) as u64;
        self.full
            && age <= max_age
            && (self.confidence as u128) * (shonin_driver::BPS as u128)
                <= (self.price as u128) * band_bps as u128
    }
}
