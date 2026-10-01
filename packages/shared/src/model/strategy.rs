//! An agent: one frozen bundle, its vault, and everything shown beside it.

use serde::{Deserialize, Serialize};
use time::Date;

use super::activity::{Kill, Trade, VaultEvent};
use super::attestation::Attestation;
use super::bundle::Bundle;
use super::common::Point;
use super::health::Health;
use super::review::Review;
use super::vault::Vault;
use super::venue::Leg;

/// Where an activated agent stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// Live, with 30 days of history: on the leaderboard.
    Listed,
    /// Live, but its record is still short: listed separately, labelled new.
    New,
    /// A claim went unserved: no new deposits, only unwinding.
    Frozen,
    /// Stopped by the admin key, with the reason on the page.
    Killed,
    /// Nothing left to do. Anyone still in withdraws from cash.
    Closed,
}

impl Standing {
    pub fn label(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::New => "new",
            Self::Frozen => "frozen",
            Self::Killed => "killed",
            Self::Closed => "closed",
        }
    }

    /// Whether it is trading and taking deposits.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Listed | Self::New)
    }
}

/// The daily series a record is read from, and everything derived from it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Value per share, day by day.
    pub value_per_share: Vec<Point>,
    /// Peak-to-trough fall on each day, as a negative fraction.
    pub drawdown: Vec<Point>,
    /// Capital behind the strategy, day by day.
    pub aum: Vec<Point>,
    /// Return in each calendar month covered by the series.
    pub monthly: Vec<Point>,
    pub trades: u32,
    /// What the vault held on average, and at its peak, over the record.
    pub average_aum: f64,
    pub peak_aum: f64,
}

/// One activated agent: its bundle, its vault and everything shown beside it.
/// A changed agent is a new agent with its own id and vault, never an edit of
/// this one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strategy {
    /// Assigned when its author paid to publish, unique on its vault chain.
    pub id: u64,
    /// The vault chain and the id together: `base-12`. What the URL uses.
    pub slug: String,
    /// The title and description from its `shonin.toml`, shown as the
    /// author's words.
    pub title: String,
    pub description: String,
    /// What it promises about itself, in the author's words. The review
    /// checks the code against them.
    pub claims: Vec<String>,
    /// The author's handle.
    pub author: String,
    pub standing: Standing,
    pub registered: Date,
    pub days_live: u32,
    pub bundle: Bundle,
    /// The vault behind it. A failed build's vault is closed and empty.
    pub vault: Vault,
    pub legs: Vec<Leg>,
    pub review: Option<Review>,
    pub health: Option<Health>,
    pub attestation: Option<Attestation>,
    pub record: Option<Record>,
    pub trades: Vec<Trade>,
    pub events: Vec<VaultEvent>,
    pub kill: Option<Kill>,
    /// Who can't deposit, by code: US and SANCTIONED on every vault, plus
    /// each venue's exclusions and the author's additions.
    pub restricted: Vec<String>,
    /// The drivers it trades through and the bridges it uses, by name.
    pub drivers: Vec<String>,
    pub bridges: Vec<String>,
}

impl Strategy {
    /// What people call it: the title its author gave it.
    pub fn name(&self) -> &str {
        &self.title
    }

    /// What the vault is worth today.
    pub fn capital(&self) -> f64 {
        self.vault.value
    }

    /// Days an agent runs live before it is listed rather than new.
    pub const DAYS_NEW: u32 = crate::record::WINDOW_DAYS;

    /// The slug for an id on a vault chain.
    pub fn slug_for(chain: super::common::Chain, id: u64) -> String {
        format!("{}-{id}", chain.name())
    }
}
