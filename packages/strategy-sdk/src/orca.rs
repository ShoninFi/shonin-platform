//! What an agent says to the `orca` driver, and what it hears back.
//!
//! The driver provides liquidity in Orca's concentrated-liquidity pools on
//! Solana, and swaps in them, for the vault's station. Each pool it lists
//! pairs one token against the vault's dollar, and is named by that token
//! (`SOL/USD`). A position is liquidity between two prices: while the pool's
//! price is between them it earns the pool's fee on trades and holds some of
//! both; below it holds only the token, above it only dollars.
//!
//! Every action is checked by the executor half against the pools the
//! station's pinned driver lists, and by the station's guard before the
//! station signs, so whatever an agent asks, the tokens and the dollars only
//! ever move between the pool and the station's own accounts.
//!
//! Prices are dollars per whole token. Amounts are whole tokens and dollars,
//! as exact decimals.

use serde::{Deserialize, Serialize};

use crate::payload::Decimal;

/// Which way a swap goes, from the pool's token's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    /// Pay dollars for the token.
    Buy,
    /// Sell the token for dollars.
    Sell,
}

/// What a strategy submits. Each action of one `submit` is one station call,
/// in the order given, each decided on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// A new position in `pool` between `lower_price` and `upper_price`
    /// (rounded outward to the pool's ticks), funded with up to `token` of
    /// the station's tokens and up to `dollars` of its dollars, as much
    /// liquidity as both allow at the pool's price now. Refused unless the
    /// pool's price is inside the band of the oracle's.
    Open {
        pool: String,
        lower_price: Decimal,
        upper_price: Decimal,
        token: Decimal,
        dollars: Decimal,
    },
    /// More liquidity in a position the station holds, from up to `token`
    /// and `dollars`, as `Open` does.
    Add {
        position: String,
        token: Decimal,
        dollars: Decimal,
    },
    /// Take `fraction` (of one) of a position's liquidity out, and its fees
    /// with it, back to the station's own accounts.
    Remove { position: String, fraction: Decimal },
    /// A position's fees, to the station's own accounts.
    Collect { position: String },
    /// An empty position, closed: its rent goes back to the key that paid it.
    Close { position: String },
    /// Swap `amount` in (dollars for a buy, tokens for a sell) at no worse
    /// than `limit_price`; what can't fill by then doesn't.
    Swap {
        pool: String,
        side: Side,
        amount: Decimal,
        limit_price: Decimal,
    },
}

/// What came back inside an accepted verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Receipt {
    Opened {
        position: String,
        lower_price: Decimal,
        upper_price: Decimal,
    },
    /// Tokens and dollars into (positive) or out of (negative) the pool.
    Moved {
        token: Decimal,
        dollars: Decimal,
    },
    Closed {
        position: String,
    },
}

/// Something that happened to the station's accounts at the venue: a reason
/// to read the account again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountEvent {
    /// A pool's price moved since the last look.
    Price { pool: String, price: Decimal },
    /// A station call through the driver landed.
    Changed,
}

/// One pool the driver lists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolState {
    /// `SOL/USD`.
    pub pool: String,
    /// The token's symbol.
    pub token: String,
    /// The pool's own price now.
    pub price: Decimal,
    /// The oracle's price, which the station values at.
    pub oracle_price: Decimal,
    /// When the oracle published it, in seconds.
    pub oracle_time: u64,
    /// The fee a swap pays, as a fraction.
    pub fee: Decimal,
    /// The pool's price step: positions' prices are rounded to these.
    pub tick_spacing: u16,
}

/// One position the station holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PositionState {
    /// Its id: the position's own address.
    pub position: String,
    pub pool: String,
    pub lower_price: Decimal,
    pub upper_price: Decimal,
    /// The pool's own measure of it, as a decimal string of a whole number.
    pub liquidity: String,
    /// What it holds at the pool's price now.
    pub token: Decimal,
    pub dollars: Decimal,
    /// Fees it has earned and not yet collected.
    pub fee_token: Decimal,
    pub fee_dollars: Decimal,
    /// Whether the pool's price is between its prices, so it earns fees.
    pub in_range: bool,
}

/// What `account-state` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    /// The whole vault at mark: its cash and everything its station holds.
    pub vault_value: Decimal,
    /// The vault's cash, on its own contract.
    pub idle: Decimal,
    /// The station's idle dollars, which an `Open`, `Add` or buy draws on.
    pub station_idle: Decimal,
    /// What the station holds at the venue at the oracle's prices.
    pub value: Decimal,
    /// What it would fetch for someone leaving.
    pub exit_value: Decimal,
    /// The station's own tokens at the venue, outside any position, by
    /// symbol.
    pub tokens: Vec<(String, Decimal)>,
    pub positions: Vec<PositionState>,
    pub pools: Vec<PoolState>,
    /// The most positions the driver lets the station hold at once.
    pub max_positions: u8,
}

impl AccountState {
    pub fn token(&self, symbol: &str) -> Decimal {
        self.tokens
            .iter()
            .find(|(held, _)| held == symbol)
            .map(|(_, amount)| *amount)
            .unwrap_or_default()
    }

    pub fn pool(&self, pool: &str) -> Option<&PoolState> {
        self.pools.iter().find(|state| state.pool == pool)
    }
}

/// The driver's name in `shonin.toml`.
pub const DRIVER: &str = "orca";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_payload, to_payload};

    #[test]
    fn an_action_reads_back_as_it_was_written() {
        let action = Action::Open {
            pool: "SOL/USD".into(),
            lower_price: Decimal::from(100),
            upper_price: Decimal::from(120),
            token: Decimal::from(1),
            dollars: Decimal::from(110),
        };
        let back: Action = from_payload(&to_payload(&action)).unwrap();
        assert_eq!(back, action);
    }
}
