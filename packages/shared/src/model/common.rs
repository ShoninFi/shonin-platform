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

/// How far along an audit of one contract or image is. Shown on every vault
/// page, because vaults open before every audit is finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditStatus {
    Audited,
    InProgress,
    Unaudited,
}

impl AuditStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Audited => "audited",
            Self::InProgress => "in progress",
            Self::Unaudited => "unaudited",
        }
    }
}

/// One line of the audit table: what was audited, and how far along it is.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Audit {
    pub subject: String,
    pub status: AuditStatus,
    /// Who did it, when it landed, or what is still outstanding.
    pub note: String,
}

/// A point in a daily series: the day, and the value on it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Point {
    pub date: Date,
    pub value: f64,
}

/// A window of a series, summarised the way the record pages show it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Performance {
    /// Annualised return of the raw series.
    pub raw_annualised: f64,
    /// Annualised return after rescaling to the platform's volatility target.
    pub normalised_annualised: f64,
    /// Realised annualised volatility of the raw series.
    pub volatility: f64,
    /// Deepest peak-to-trough fall, as a negative fraction.
    pub max_drawdown: f64,
    /// Total return over the whole series.
    pub total_return: f64,
    pub days: u32,
}
