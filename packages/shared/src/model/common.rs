//! Small types the rest of the model is built from: chains, money, hashes,
//! audit status, and the shape of a dated point in a series.

use serde::{Deserialize, Serialize};
use time::Date;

/// A chain a vault lives on, by the name a strategy's `shonin.toml` gives it.
///
/// Which actual chain a name means (a local devnet, a testnet or a mainnet)
/// is the network's decision, made in `config/chains.toml`; the name stays
/// the same across all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chain {
    Base,
    Hyperliquid,
    Robinhood,
    Solana,
}

impl Chain {
    pub const ALL: [Chain; 4] = [Self::Base, Self::Hyperliquid, Self::Robinhood, Self::Solana];

    /// The name `shonin.toml` uses: `[vault] chain = "base"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Hyperliquid => "hyperliquid",
            Self::Robinhood => "robinhood",
            Self::Solana => "solana",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Base => "Base",
            Self::Hyperliquid => "Hyperliquid",
            Self::Robinhood => "Robinhood Chain",
            Self::Solana => "Solana",
        }
    }

    /// The dollar a vault on this chain settles in.
    pub fn dollar(self) -> &'static str {
        match self {
            Self::Base | Self::Hyperliquid | Self::Solana => "USDC",
            Self::Robinhood => "USDG",
        }
    }

    pub fn from_name(name: &str) -> Option<Chain> {
        Self::ALL.into_iter().find(|chain| chain.name() == name)
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Chain {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::from_name(name).ok_or_else(|| {
            format!("no vault chain is called {name:?}: base, hyperliquid, robinhood or solana")
        })
    }
}

/// A point in a daily series: the day, and the value on it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Point {
    pub date: Date,
    pub value: f64,
}

/// A record, summarised the way the leaderboard shows it: plain facts anyone
/// can recompute from the vault's value per share.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Performance {
    /// The return over the last 30 days, once there are 30 days of history.
    pub return_30d: Option<f64>,
    pub return_since_open: f64,
    /// The largest fall in value per share from an earlier high, as a
    /// negative fraction.
    pub deepest_drop: f64,
    pub days: u32,
}
