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
///
/// Every order of one `submit` goes to the venue in one transaction, in the
/// order given, and each is decided on its own: one refused or unfilled
/// leaves the others' fills standing, so a submission is not atomic, and
/// each order gets its own verdict. Only when the transaction as a whole is
/// refused (the key isn't approved, say) are the orders sent again one at a
/// time, so each is still decided on its own; a transaction that was sent and
/// never answered is not sent again, and every order in it is `unknown`.
/// Sending the orders a strategy means together in one `submit` is what keeps
/// the vault's gas down.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Place(Order),
}

/// What `account-state` is asked: the markets the strategy follows.
///
/// The venue's market operator posts quotes and funding for every market it
/// lists, every few seconds. A strategy is woken only for the markets it
/// follows: those named here, those it holds, and those it has sent an
/// order for. An empty query (as any payload that isn't a `Query` is read)
/// follows every market, from then on, as a strategy that names none always
/// has. Naming them also keeps the reply's `markets` to those and the ones
/// held, which are fewer reads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub markets: Vec<String>,
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
}

/// Something that happened to the vault's account, or to a market the
/// strategy follows (see [`Query`]), as the venue saw it. Each is a reason to
/// read the account again rather than the whole story: the account read
/// carries every market's latest quote and funding.
///
/// Each look at the chain wakes a strategy as little as it can: its fills,
/// and funding posted for the markets it follows, are always said; a new
/// quote for a market it follows is said as [`AccountEvent::Prices`] only
/// when the look brought nothing else, since the operator posts quotes and
/// funding together and the other event already says to look.
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
    /// A market's latest quote, when nothing else in the same look said to
    /// read the account.
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
    /// The vault's cash, on its own contract.
    pub idle: Decimal,
    /// The station's idle dollars, beside the venue account.
    pub station_idle: Decimal,
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
