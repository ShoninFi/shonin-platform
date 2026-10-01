//! What a vault did: its trades on venues, and the events on its contract.
//! Both are public, which is what makes a record impossible to backfill.

use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
}

/// One fill on a venue, as anyone reading the chain would see it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub at: PrimitiveDateTime,
    pub venue: String,
    pub market: String,
    pub side: Side,
    pub size: f64,
    pub price: f64,
    pub notional: f64,
    /// `reduce-only`, `maker`, `taker`, or what the order was for.
    pub kind: String,
    pub reference: String,
}

/// Something the vault contract did, from its own event log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultEvent {
    pub at: PrimitiveDateTime,
    pub kind: EventKind,
    pub summary: String,
    /// Money involved, where the event has an amount.
    pub amount: Option<f64>,
    pub reference: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Deposited,
    Withdrawn,
    ClaimBooked,
    ClaimSettled,
    Crystallised,
    CapitalMoved,
    TankCharged,
    TankFunded,
    Frozen,
    Killed,
    Blessed,
    Valued,
    Retired,
    Refunded,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Deposited => "deposited",
            Self::Withdrawn => "withdrawn",
            Self::ClaimBooked => "claim booked",
            Self::ClaimSettled => "claim settled",
            Self::Crystallised => "crystallised",
            Self::CapitalMoved => "capital moved",
            Self::TankCharged => "tank charged",
            Self::TankFunded => "tank funded",
            Self::Frozen => "frozen",
            Self::Killed => "killed",
            Self::Blessed => "keys blessed",
            Self::Valued => "valued",
            Self::Retired => "retired",
            Self::Refunded => "deposit returned",
        }
    }
}

/// Why a vault was stopped, announced on its page and on the public feed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kill {
    pub at: PrimitiveDateTime,
    pub reason: String,
    /// What happened to the money afterwards.
    pub outcome: String,
    pub reference: String,
}
