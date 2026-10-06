//! What an agent says to the `robinhood` driver, and what it hears back.
//!
//! The driver trades Robinhood Chain's stock tokens on Uniswap from a Safe
//! the vault's station owns. Everything an agent can ask for is here: swap a
//! listed stock token for the dollar or the dollar for one, for no less than
//! a minimum it names. The executor checks the minimum against the token's
//! Chainlink price before it signs, and the Safe's role checks it again
//! on-chain, so a swap far from the feed is refused twice. Nothing here can
//! name a recipient, a pool or a token outside the driver's list.
//!
//! This module needs only `serde` and `fastnum`, so an agent compiled to
//! WebAssembly can use it with the crate's default features off.

use serde::{Deserialize, Serialize};

/// The decimal every driver's payloads use.
pub type Decimal = fastnum::D128;

/// The driver's name, as `shonin.toml` writes it.
pub const DRIVER: &str = "robinhood";

/// The dollar's symbol: USDG, the chain's native dollar.
pub const DOLLAR: &str = "USDG";

/// What a strategy submits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Spend `amount` of the dollar on stock token `token`, receiving at
    /// least `min_tokens`.
    Buy {
        token: String,
        amount: Decimal,
        min_tokens: Decimal,
    },
    /// Sell `tokens` of stock token `token` for at least `min_amount`
    /// dollars.
    Sell {
        token: String,
        tokens: Decimal,
        min_amount: Decimal,
    },
}

/// What came back inside an accepted verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Receipt {
    /// The swap landed: what was spent and what came back.
    Swapped {
        token: String,
        spent: Decimal,
        received: Decimal,
        transaction: String,
    },
}

/// What `account-state` is asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Query {
    /// The Safe's holdings, priced.
    Holdings,
    /// Every listed token's prices: its feed's and its pool's.
    Prices,
}

/// One listed token's prices, as `Query::Prices` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenPrice {
    pub token: String,
    /// Chainlink's price for the token (the multiplier already applied).
    pub feed_price: Decimal,
    /// When the feed last moved, in seconds since the Unix epoch.
    pub feed_updated_at: u64,
    /// What the token's pool says, averaged over the driver's window.
    pub market_price: Decimal,
    /// Shares of the underlying per token.
    pub multiplier: Decimal,
    /// The issuer has paused the token's oracle.
    pub paused: bool,
}

/// One stock token the Safe holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Holding {
    pub token: String,
    /// In shares of the token (its own units, not multiplied: the
    /// multiplier only matters for what a share is a claim on).
    pub tokens: Decimal,
    /// The token's Chainlink price, which already applies the multiplier.
    pub feed_price: Decimal,
    /// When the feed last moved, in seconds since the Unix epoch.
    pub feed_updated_at: u64,
    /// Shares of the underlying per token, from the token's multiplier:
    /// for display, never for valuing.
    pub multiplier: Decimal,
}

/// What `account-state` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    /// The Safe's address.
    pub safe: String,
    /// Idle dollars in the Safe.
    pub dollars: Decimal,
    pub holdings: Vec<Holding>,
}

/// Something that happened to the Safe, read from the chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountEvent {
    /// Tokens moved into or out of the Safe: a swap, a deposit, a close.
    Moved {
        token: String,
        /// Positive when the Safe received.
        amount: Decimal,
        transaction: String,
        block: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_round_trips_through_its_canonical_payload() {
        let action = Action::Buy {
            token: "NVDA".into(),
            amount: "100.5".parse().unwrap(),
            min_tokens: "0.41".parse().unwrap(),
        };
        let mut bytes = Vec::new();
        ciborium::into_writer(&action, &mut bytes).unwrap();
        let back: Action = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back, action);
    }
}
