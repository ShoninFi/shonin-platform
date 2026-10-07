//! What an agent says to the `hyperliquid` driver, and what it hears back.
//!
//! These types are all a strategy sees of Hyperliquid: it builds an
//! [`Action`], encodes it with [`to_payload`](crate::to_payload), and submits
//! it through the driver named [`DRIVER`]; the driver's executor half decodes
//! it inside the measured image. The list of actions is the list of what the
//! executor will sign for the vault's account: trading, and moving dollars
//! between the account's own perp and spot balances. Nothing here can name
//! another account, a venue vault, an agent or the account's balance mode, so
//! no payload can ask for one.
//!
//! Markets are named as the venue names them: a perp by its coin (`ETH`), a
//! spot pair against the dollar by its base token (`HYPE/USDC`).

use serde::{Deserialize, Serialize};

pub use crate::payload::Decimal;

/// The driver's name, as `shonin.toml` writes it.
pub const DRIVER: &str = "hyperliquid";

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
    /// Added to the book only, never taking: refused if it would cross.
    PostOnly,
    /// Good until cancelled.
    Gtc,
    /// Immediate or cancel.
    Ioc,
}

/// An order on one market: a perp by its coin (`ETH`), or a spot pair
/// against the dollar by its base token (`HYPE/USDC`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub market: String,
    pub side: Side,
    pub size: Decimal,
    pub limit_price: Decimal,
    pub tif: Tif,
    /// Only ever shrinks a perp position, never past flat.
    pub reduce_only: bool,
    /// The agent's own id for the order, so it can cancel it before the
    /// venue's id is known.
    pub client_id: Option<u128>,
}

/// Which order: the venue's id, or the agent's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrderRef {
    Venue(u64),
    Client(u128),
}

/// Which of the account's own balances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Balance {
    Perp,
    Spot,
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
    /// Replace a resting order with `order`.
    Modify {
        order_ref: OrderRef,
        order: Order,
    },
    /// A perp's leverage, always cross margin: isolated margin moves money
    /// out of the account's shared balance, and the station values cross.
    Leverage {
        market: String,
        leverage: u32,
    },
    /// Dollars between the account's own perp and spot balances.
    MoveDollars {
        from: Balance,
        amount: Decimal,
    },
    /// Cancel every open order at `at_ms` unless asked again before; `None`
    /// clears it. A dead man's switch for an agent that may stop.
    CancelAllAt {
        at_ms: Option<u64>,
    },
}

/// What came back inside an accepted verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Receipt {
    /// Resting on the book under the venue's id.
    Resting { order_id: u64 },
    Filled {
        order_id: u64,
        size: Decimal,
        average_price: Decimal,
    },
    /// Taken by the venue, nothing more to say (a cancel, a leverage change).
    Done,
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
        fee: Decimal,
        order_id: u64,
        time_ms: u64,
    },
    /// An order's state changed: `open`, `filled`, `canceled`, `rejected`
    /// and the venue's other words for it.
    Order {
        market: String,
        order_id: u64,
        client_id: Option<u128>,
        status: String,
        time_ms: u64,
    },
    Funding {
        market: String,
        /// Positive when the account was paid.
        amount: Decimal,
        time_ms: u64,
    },
    Liquidated {
        time_ms: u64,
    },
}

/// What `account-state` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    /// The whole vault at mark: its cash and everything its station holds,
    /// this account included. What a strategy sizes against.
    pub vault_value: Decimal,
    /// The vault's cash, on its own contract.
    pub idle: Decimal,
    /// The station's idle dollars on HyperEVM, beside the account: what a
    /// move into the account can take without touching the vault's cash.
    /// Zero in a reply from an executor that doesn't report it, so a payload
    /// without it still reads; and an agent built before it reads a payload
    /// with it, since a field it doesn't know is passed over.
    #[serde(default)]
    pub station_idle: Decimal,
    /// The account's perp side at mark.
    pub account_value: Decimal,
    pub withdrawable: Decimal,
    pub positions: Vec<Position>,
    pub spot: Vec<SpotBalance>,
    pub open_orders: Vec<OpenOrder>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub market: String,
    /// Positive is long.
    pub size: Decimal,
    pub entry_price: Option<Decimal>,
    pub leverage: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpotBalance {
    pub token: String,
    pub total: Decimal,
    pub hold: Decimal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenOrder {
    pub market: String,
    pub order_id: u64,
    pub side: Side,
    pub size: Decimal,
    pub limit_price: Decimal,
}
