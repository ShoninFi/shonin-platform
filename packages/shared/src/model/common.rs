//! Small types the rest of the model is built from: chains, money, hashes,
//! audit status, and the shape of a dated point in a series.

use serde::{Deserialize, Serialize};
use time::Date;

/// A chain a vault, an entry or a registry lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chain {
    HyperEvm,
    RobinhoodChain,
    Solana,
    /// The development chain the platform runs against locally.
    Local,
}

impl Chain {
    pub fn label(self) -> &'static str {
        match self {
            Self::HyperEvm => "HyperEVM",
            Self::RobinhoodChain => "Robinhood Chain",
            Self::Solana => "Solana",
            Self::Local => "Local devnet",
        }
    }

    /// The dollar the chain settles in.
    pub fn asset(self) -> &'static str {
        match self {
            Self::HyperEvm => "USDC",
            Self::RobinhoodChain => "USDG",
            Self::Solana => "USDC",
            Self::Local => "USDC",
        }
    }

    pub fn all() -> [Chain; 4] {
        [
            Self::HyperEvm,
            Self::RobinhoodChain,
            Self::Solana,
            Self::Local,
        ]
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
