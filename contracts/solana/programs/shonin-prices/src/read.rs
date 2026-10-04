//! Reading the price list, from another program or from off the chain.
//!
//! The history is read in place, period by period, so pricing a recent
//! interval costs the same however long the history grows.

use anchor_lang::prelude::*;
use anchor_lang::Discriminator;
use shonin_common::{math::U256, DAY};

use crate::{cost, Allowances, PriceList, PricePeriod, PricesError, UnitPrices, PERIOD_SIZE};

/// The dollar has 6 decimals; prices are dollars scaled by 1e18.
pub const TO_ASSET: u64 = 1_000_000_000_000;

/// The price list's address.
pub fn prices_address() -> Pubkey {
    Pubkey::find_program_address(&[crate::PRICES_SEED], &crate::ID).0
}

/// The price list's bytes, checked to be the price list.
pub struct History<'a> {
    data: &'a [u8],
    len: usize,
}

impl<'a> History<'a> {
    /// The price list held at `key`, owned by `owner`, with these bytes.
    pub fn new(key: &Pubkey, owner: &Pubkey, data: &'a [u8]) -> Result<Self> {
        require!(
            key == &prices_address()
                && owner == &crate::ID
                && data.len() >= PriceList::PERIODS_AT
                && data[..8] == *PriceList::DISCRIMINATOR,
            PricesError::NotThePriceList
        );
        let len = u32::from_le_bytes(
            data[PriceList::LENGTH_AT..PriceList::PERIODS_AT]
                .try_into()
                .map_err(|_| error!(PricesError::NotThePriceList))?,
        ) as usize;
        require!(
            len >= 1 && data.len() >= PriceList::space(len),
            PricesError::NotThePriceList
        );
        Ok(Self { data, len })
    }

    pub fn admin(&self) -> Pubkey {
        Pubkey::try_from(&self.data[8..40]).unwrap_or_default()
    }

    pub fn notice(&self) -> u64 {
        u64::from_le_bytes(self.data[40..48].try_into().unwrap_or_default())
    }

    /// How many periods there are; never none.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn at(&self, i: usize) -> &[u8] {
        let start = PriceList::space(i);
        &self.data[start..start + PERIOD_SIZE]
    }

    pub fn effective_from(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.at(i)[..8].try_into().unwrap_or_default())
    }

    pub fn period(&self, i: usize) -> Result<PricePeriod> {
        Ok(PricePeriod::deserialize(&mut &self.at(i)[..])?)
    }

    /// The prices in force at `now`: the latest period begun by then.
    pub fn current(&self, now: u64) -> Result<UnitPrices> {
        Ok(self.period(self.last_begun_by(now))?.prices)
    }

    /// The last period that began at or before `time`, found by halving.
    /// The first period began at zero, so there always is one.
    fn last_begun_by(&self, time: u64) -> usize {
        let (mut lo, mut hi) = (0usize, self.len);
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.effective_from(mid) <= time {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The most the allowances could cost between two times, in the
    /// dollar's units, splitting the interval at every price change inside
    /// it. Only the periods the interval touches are read: from the last one
    /// that began at or before `from` to the first that begins at or after
    /// `to`. Rounds down, in the tank's favour.
    pub fn cost_of(&self, allowances: &Allowances, from: u64, to: u64) -> Result<U256> {
        if to <= from {
            return Ok(U256::zero());
        }
        let mut total = U256::zero();
        let mut i = self.last_begun_by(from);
        while i < self.len && self.effective_from(i) < to {
            let start = self.effective_from(i);
            let end = if i + 1 < self.len {
                self.effective_from(i + 1)
            } else {
                u64::MAX
            };
            // The overlap of [from, to) with this period.
            let lo = from.max(start);
            let hi = to.min(end);
            if hi > lo {
                total = total
                    .checked_add(cost(allowances, &self.period(i)?.prices, hi - lo))
                    .ok_or(PricesError::Overflow)?;
            }
            i += 1;
        }
        Ok(total / U256::from(TO_ASSET))
    }

    /// A day at the allowances, at the prices in force at `now`.
    pub fn daily_cost(&self, allowances: &Allowances, now: u64) -> Result<U256> {
        Ok(cost(allowances, &self.current(now)?, DAY) / U256::from(TO_ASSET))
    }
}
