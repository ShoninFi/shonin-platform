//! Venues, custody and how a vault's value on each one is arrived at.

use serde::{Deserialize, Serialize};

use super::common::Chain;

/// How strongly the venue itself stops a vault's keys from sending funds
/// anywhere but the vault's own accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Custody {
    /// The venue holds the line: the key can trade and nothing else.
    VenueEnforced,
    /// Contracts on the chain hold the line: scoped roles and session keys.
    ChainEnforced,
}

impl Custody {
    pub fn label(self) -> &'static str {
        match self {
            Self::VenueEnforced => "venue-enforced",
            Self::ChainEnforced => "chain-enforced",
        }
    }

    /// Who refuses a withdrawal to a stranger.
    pub fn enforced_by(self) -> &'static str {
        match self {
            Self::VenueEnforced => "the venue",
            Self::ChainEnforced => "contracts on the chain",
        }
    }
}

/// Who turns a venue's positions back into cash when a depositor leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitPath {
    /// The vault contract itself; anyone can send the transaction.
    Contract,
    /// An attested unwinder, which any allowlisted operator can run.
    Unwinder,
}

impl ExitPath {
    pub fn label(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Unwinder => "unwinder",
        }
    }
}

/// How a vault knows what an account on this venue is worth.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Valuation {
    /// The contract reads it from the chain it lives on, in the same
    /// transaction. `source` names the precompile, feed or oracle.
    OnChain { source: String },
    /// A contract on another chain computes it there and posts it home.
    Posted { carrier: String, chain: Chain },
    /// A measured image reads the account from the venue's API and signs it.
    AttestedRead { image: String },
}

impl Valuation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OnChain { .. } => "computed on-chain",
            Self::Posted { .. } => "posted from another chain",
            Self::AttestedRead { .. } => "attested read",
        }
    }
}

/// Whether the venue is behaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VenueStatus {
    Ok,
    Suspended,
}

/// The part of a vault's capital that sits on one venue, and everything a
/// depositor is shown about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Leg {
    pub venue: String,
    pub chain: Chain,
    pub custody: Custody,
    pub exit_path: ExitPath,
    /// What happens if nobody serves a claim here.
    pub last_resort: Option<String>,
    /// Fraction of the vault's value held on this venue, 0.0 to 1.0.
    pub share_of_value: f64,
    pub valuation: Valuation,
    /// The vault's account on this venue.
    pub account: String,
    /// What the executor holds, and what it can do with it.
    pub key: String,
    pub can: Vec<String>,
    pub cannot: Vec<String>,
    pub status: VenueStatus,
    /// Shown when the venue is suspended, or when something needs saying.
    pub note: Option<String>,
}

/// A vault's exit class: how its claims get served.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitClass {
    /// Every venue is reachable by the contract. Exit needs nobody.
    Direct,
    /// At least one venue can only be unwound off-chain.
    Served { operators: u32 },
}

impl ExitClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Served { .. } => "served",
        }
    }

    pub fn summary(self) -> String {
        match self {
            Self::Direct => {
                "Exit needs nobody: the contract serves every claim itself.".to_string()
            }
            Self::Served { operators } => format!(
                "One venue is unwound off-chain. {operators} operator{} can serve a claim.",
                if operators == 1 { "" } else { "s" }
            ),
        }
    }
}
