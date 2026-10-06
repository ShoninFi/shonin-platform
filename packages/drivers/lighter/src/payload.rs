//! What an agent says to the `lighter` driver, and what it hears back.
//!
//! These types are all a strategy sees of Lighter: it builds an [`Action`],
//! encodes it as a payload (canonical CBOR, as every driver's payloads are),
//! and the driver's executor half decodes it inside the measured image. The
//! list of actions is the list of what the executor will sign with the
//! station's Lighter key: orders, cancels and modifications on the
//! station's own master account. Nothing here can name another account, a
//! public pool, a sub-account, a transfer or a withdrawal, so no payload can
//! ask the key to do any of the things it could otherwise do.
//!
//! This module needs only `serde` and `fastnum`, so an agent compiled to
//! WebAssembly can use it with the crate's default features off.

use serde::{Deserialize, Serialize};

/// The decimal every driver's payloads use.
pub type Decimal = fastnum::D128;

/// The driver's name, as `shonin.toml` writes it.
pub const DRIVER: &str = "lighter";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Buy,
    Sell,
}

/// How long an order stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tif {
    /// Added to the book only, never taking.
    PostOnly,
    /// Stands until `expiry_ms` (Lighter allows up to 28 days) or a cancel.
    GoodTill { expiry_ms: u64 },
    /// Immediate or cancel, at the limit price or better.
    Ioc,
}

/// An order on one perp market, by its symbol (`SPY`, `NVDA`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub market: String,
    pub side: Side,
    /// In units of the underlying (shares, coins).
    pub size: Decimal,
    pub limit_price: Decimal,
    pub tif: Tif,
    /// Only ever shrinks a position, never past flat.
    pub reduce_only: bool,
    /// The agent's own id for the order (1 to 2^48 - 1), so it can cancel it
    /// before it knows the venue's.
    pub client_id: Option<u64>,
}

/// Which order: the venue's index, or the agent's own id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrderRef {
    Venue(u64),
    Client(u64),
}

/// What a strategy submits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Place(Order),
    Cancel {
        market: String,
        order: OrderRef,
    },
    /// Change a resting order's size and price.
    Modify {
        market: String,
        order: OrderRef,
        size: Decimal,
        limit_price: Decimal,
    },
    /// Cancel every resting order on the account.
    CancelAll,
}

/// What came back inside an accepted verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Receipt {
    /// The exchange took the transaction; what it did with it arrives as
    /// account events.
    Sent { tx_hash: String },
}

/// What `account-state` is asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Query {
    Account,
}

/// One perp position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub market: String,
    /// Positive is long.
    pub size: Decimal,
    pub entry_price: Decimal,
    pub unrealized_pnl: Decimal,
}

/// What `account-state` answers: the station's master account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    pub account_index: u64,
    /// The account's worth at the mark.
    pub equity: Decimal,
    /// What it could withdraw or put up as margin now.
    pub available: Decimal,
    pub positions: Vec<Position>,
}

/// Something that happened to the account, as the venue reported it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountEvent {
    Filled {
        market: String,
        side: Side,
        size: Decimal,
        price: Decimal,
        time_ms: u64,
    },
    Order {
        market: String,
        order_index: u64,
        client_id: Option<u64>,
        status: String,
        time_ms: u64,
    },
    Funding {
        market: String,
        /// Positive when the account was paid.
        amount: Decimal,
        time_ms: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_round_trips_through_its_canonical_payload() {
        let action = Action::Place(Order {
            market: "SPY".into(),
            side: Side::Sell,
            size: "1.25".parse().unwrap(),
            limit_price: "775.5".parse().unwrap(),
            tif: Tif::GoodTill { expiry_ms: 1 },
            reduce_only: false,
            client_id: Some(7),
        });
        let mut bytes = Vec::new();
        ciborium::into_writer(&action, &mut bytes).unwrap();
        let back: Action = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back, action);
    }
}
