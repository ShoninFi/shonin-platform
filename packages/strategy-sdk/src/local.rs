//! What an agent says to the `local` driver, and what it hears back.
//!
//! These are the typed payloads for the stand-in venue the platform runs
//! against on local chains and testnets. Each driver has its own crate of
//! these; its executor half decodes them inside the measured image and
//! refuses anything that does not decode or names an account the vault does
//! not own.

use serde::{Deserialize, Serialize};

use crate::payload::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Buy,
    Sell,
}

/// One of the vault's own accounts. There are two, and nothing else exists to
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Account {
    /// The balance sitting on the vault's own contract.
    Idle,
    /// The vault's account on the venue.
    Venue,
}

/// An order. Every order is immediate-or-cancel against the venue's current
/// bid and ask, and `limit_price` is the worst price it may fill at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub market: String,
    pub side: Side,
    pub size: Decimal,
    pub limit_price: Decimal,
    /// Only ever shrinks a position, and never past flat.
    pub reduce_only: bool,
}

/// What a strategy submits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Place(Order),
    /// Move the vault's own money between the vault's own accounts.
    MoveCapital {
        from: Account,
        to: Account,
        amount: Decimal,
    },
}

/// What came back inside an accepted verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Receipt {
    Filled {
        market: String,
        side: Side,
        size: Decimal,
        price: Decimal,
        notional: Decimal,
    },
    Moved {
        from: Account,
        to: Account,
        amount: Decimal,
    },
}

/// Something that happened to the vault's account, as the venue saw it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountEvent {
    Filled {
        market: String,
        side: Side,
        size: Decimal,
        price: Decimal,
        notional: Decimal,
        reduce_only: bool,
    },
    /// Longs paid shorts this much for each unit of size.
    Funding {
        market: String,
        paid_per_unit: Decimal,
    },
    Prices(MarketState),
}

/// What the vault holds in one market.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PositionState {
    pub market: String,
    /// Positive is long, negative is short.
    pub size: Decimal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketState {
    pub market: String,
    pub is_perp: bool,
    pub mark: Decimal,
    pub oracle: Decimal,
    pub bid: Decimal,
    pub ask: Decimal,
    /// Dollars per unit of size that longs have paid shorts since the market
    /// opened. The difference between two readings is what a position earned
    /// or owed over that stretch.
    pub funding_index: Decimal,
    /// What longs are paying shorts now, per unit of size per day. Positive
    /// means the carry favours a short.
    pub funding_rate: Decimal,
}

/// Everything the strategy can ask the venue about its own account. The reply
/// carries the vault beside it, because a strategy sizes against the vault's
/// value rather than the venue's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    /// On the venue. Negative when a perp is held on margin.
    pub cash: Decimal,
    /// What the venue account is worth at mark.
    pub value: Decimal,
    /// What it would be worth to someone leaving.
    pub exit_value: Decimal,
    /// The whole vault at mark, including the part not on the venue.
    pub vault_value: Decimal,
    /// The part of the vault sitting on its own contract.
    pub idle: Decimal,
    /// What a perp position has to be backed by, in basis points.
    pub margin_bps: u32,
    pub positions: Vec<PositionState>,
    pub markets: Vec<MarketState>,
}

impl AccountState {
    pub fn position(&self, market: &str) -> Decimal {
        self.positions
            .iter()
            .find(|held| held.market == market)
            .map(|held| held.size)
            .unwrap_or_default()
    }

    pub fn market(&self, market: &str) -> Option<&MarketState> {
        self.markets.iter().find(|state| state.market == market)
    }
}

impl MarketState {
    /// The funding as a fraction of notional per day, which is what a carry
    /// is actually worth: dollars per unit mean nothing without the price.
    pub fn funding_fraction(&self) -> Decimal {
        if self.mark <= Decimal::from(0) {
            return Decimal::from(0);
        }
        self.funding_rate / self.mark
    }
}

/// What the vault says when a depositor is leaving: the fraction of every
/// position being closed to serve them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimPending {
    pub fraction: Decimal,
}
